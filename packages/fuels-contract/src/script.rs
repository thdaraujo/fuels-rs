use anyhow::Result;
use fuel_gql_client::fuel_tx::{Input, Output, UtxoId};
use fuel_gql_client::fuel_types::{
    bytes::padded_len_usize, AssetId, Bytes32, ContractId, Immediate18, Word,
};
use fuel_gql_client::fuel_vm::consts::VM_TX_MEMORY;
use fuel_gql_client::fuel_vm::{
    consts::{REG_CGAS, REG_ONE},
    prelude::Opcode,
};
use fuel_gql_client::{
    client::{types::TransactionStatus, FuelClient},
    fuel_tx::{Receipt, Transaction},
};
use fuels_core::constants::{DEFAULT_SPENDABLE_COIN_AMOUNT, WORD_SIZE};
use fuels_core::errors::Error;
use fuels_core::parameters::TxParameters;
use std::collections::HashSet;

use crate::contract::ContractCall;
use fuels_signers::{LocalWallet, Signer};

#[derive(Default)]
/// Specifies offsets of Opcode::CALL parameters stored in the script
/// data from which they can be loaded into registers
struct CallParamOffsets {
    pub asset_id_offset: usize,
    pub amount_offset: usize,
    pub call_data_offset: usize,
}

/// Script provides methods to create and a call/simulate a
/// script transaction that carries out contract method calls
pub struct Script {
    pub tx: Transaction,
}

#[derive(Debug, Clone)]
pub struct CompiledScript {
    pub raw: Vec<u8>,
    pub target_network_url: String,
}

impl Script {
    pub fn new(tx: Transaction) -> Self {
        Self { tx }
    }

    /// Creates a Script from a contract call. The internal Transaction is initialized
    /// with the actual script instructions, script data needed to perform the call
    /// and transaction inputs/outputs consisting of assets and contracts
    pub async fn from_contract_calls(
        calls: Vec<&ContractCall>,
        tx_parameters: &TxParameters,
        wallet: &LocalWallet,
    ) -> Self {
        let data_offset = Self::get_data_offset(calls.len());

        let (script_data, call_param_offsets) = Self::get_script_data(calls.clone(), data_offset);

        let script = Self::get_instructions(calls.clone(), call_param_offsets);

        let (inputs, outputs) = Self::get_transaction_inputs_outputs(calls.clone(), wallet).await;

        let mut tx = Transaction::script(
            tx_parameters.gas_price,
            tx_parameters.gas_limit,
            tx_parameters.byte_price,
            tx_parameters.maturity,
            script,
            script_data,
            inputs,
            outputs,
            vec![],
        );
        wallet.sign_transaction(&mut tx).await.unwrap();

        Script::new(tx)
    }

    /// Given a list of contract calls, create the actual opcodes used to call the contract
    fn get_instructions(calls: Vec<&ContractCall>, offsets: Vec<CallParamOffsets>) -> Vec<u8> {
        let num_calls = calls.len();

        let mut instructions = vec![];
        for (_, call_offsets) in (0..num_calls).zip(offsets.iter()) {
            instructions.extend(Self::get_single_call_instructions(call_offsets));
        }

        instructions.extend(Opcode::RET(REG_ONE).to_bytes());

        instructions
    }

    /// Returns script data, consisting of the following items in the given order:
    /// 1. Asset ID to be forwarded (AmountId::LEN)
    /// 2. Amount to be forwarded (1 * WORD_SIZE)
    /// 3. Contract ID (ContractID::LEN);
    /// 4. Function selector (1 * WORD_SIZE);
    /// 5. Calldata offset (optional) (1 * WORD_SIZE)
    /// 6. Encoded arguments (optional) (variable length)
    fn get_script_data(
        calls: Vec<&ContractCall>,
        data_offset: usize,
    ) -> (Vec<u8>, Vec<CallParamOffsets>) {
        let mut script_data = vec![];
        let mut param_offsets = vec![];

        let mut segment_offset = data_offset;

        for call in calls {
            param_offsets.push(CallParamOffsets {
                asset_id_offset: segment_offset,
                amount_offset: segment_offset + AssetId::LEN,
                call_data_offset: segment_offset + AssetId::LEN + WORD_SIZE,
            });

            script_data.extend(call.call_parameters.asset_id.to_vec());

            let amount = call.call_parameters.amount as Word;
            script_data.extend(amount.to_be_bytes());

            script_data.extend(call.contract_id.as_ref());

            script_data.extend(call.encoded_selector);

            // If the method call takes custom inputs or has more than
            // one argument, we need to calculate the `call_data_offset`,
            // which points to where the data for the custom types start in the
            // transaction. If it doesn't take any custom inputs, this isn't necessary.
            if call.compute_calldata_offset {
                // Offset of the script data relative to the call data
                let call_data_offset =
                    segment_offset + AssetId::LEN + WORD_SIZE + ContractId::LEN + 2 * WORD_SIZE;
                let call_data_offset = call_data_offset as Word;

                script_data.extend(&call_data_offset.to_be_bytes());
            }

            script_data.extend(call.encoded_args.clone());

            // the data segment that holds the parameters for the next call
            // begins at the original offset + the data we added so far
            segment_offset = data_offset + script_data.len();
        }

        (script_data, param_offsets)
    }

    /// Returns the VM instructions for calling a contract method
    /// We use the Opcode to call a contract: `CALL` pointing at the
    /// following registers;
    ///
    /// 0x10 Script data offset
    /// 0x11 Gas price TODO: #184
    /// 0x12 Coin amount
    /// 0x13 Asset ID
    ///
    /// Note that these are soft rules as we're picking this addresses simply because they
    /// non-reserved register.
    fn get_single_call_instructions(offsets: &CallParamOffsets) -> Vec<u8> {
        let instructions = vec![
            Opcode::MOVI(0x10, offsets.call_data_offset as Immediate18),
            Opcode::MOVI(0x12, offsets.amount_offset as Immediate18),
            Opcode::LW(0x12, 0x12, 0),
            Opcode::MOVI(0x13, offsets.asset_id_offset as Immediate18),
            Opcode::CALL(0x10, 0x12, 0x13, REG_CGAS),
        ];

        #[allow(clippy::iter_cloned_collect)]
        instructions.iter().copied().collect::<Vec<u8>>()
    }

    /// Returns the assets and contracts that will be consumed (inputs) and created (outputs)
    /// by the transaction
    async fn get_transaction_inputs_outputs(
        calls: Vec<&ContractCall>,
        wallet: &LocalWallet,
    ) -> (Vec<Input>, Vec<Output>) {
        let mut inputs: Vec<Input> = vec![];
        let mut outputs: Vec<Output> = vec![];

        // Get all unique contract ids
        let mut contract_ids = HashSet::new();
        calls.iter().for_each(|call| {
            contract_ids.insert(call.contract_id);

            if let Some(external_contracts) = call.external_contracts.clone() {
                contract_ids.extend(external_contracts);
            }
        });

        // We must associate the right external contract input to the corresponding external
        // output index (TXO)
        for (idx, contract_id) in contract_ids.into_iter().enumerate() {
            let zeroes = Bytes32::zeroed();
            let self_contract_input = Input::contract(
                UtxoId::new(Bytes32::zeroed(), idx as u8),
                zeroes,
                zeroes,
                contract_id,
            );
            inputs.push(self_contract_input);

            let external_contract_output = Output::contract(idx as u8, zeroes, zeroes);
            outputs.push(external_contract_output);
        }

        // Get all unique asset ids
        let mut asset_ids = HashSet::new();
        asset_ids.insert(AssetId::default());
        calls.iter().for_each(|call| {
            asset_ids.insert(call.call_parameters.asset_id);
        });

        let mut spendables = vec![];
        for asset_id in asset_ids.iter() {
            spendables.extend(
                wallet
                    .get_spendable_coins(asset_id, DEFAULT_SPENDABLE_COIN_AMOUNT as u64)
                    .await
                    .unwrap(),
            );

            // add asset change if any inputs are being spent
            let change_output = Output::change(wallet.address(), 0, asset_id.to_owned());
            outputs.push(change_output);
        }

        for coin in spendables {
            let input_coin = Input::coin_signed(
                UtxoId::from(coin.utxo_id),
                coin.owner.into(),
                coin.amount.0,
                coin.asset_id.into(),
                0,
                0,
            );

            inputs.push(input_coin);
        }

        calls.iter().for_each(|call| {
            if let Some(v) = call.variable_outputs.clone() {
                outputs.extend(v);
            };
        });

        (inputs, outputs)
    }

    /// Calculates the length of the script based on the number of contract calls it
    /// has to make and returns the offset at which the script data begins
    fn get_data_offset(num_calls: usize) -> usize {
        // use placeholder for call param offsets, we only care about the length
        let mut len_script =
            Script::get_single_call_instructions(&CallParamOffsets::default()).len() * num_calls;

        // to account for RET instruction which is added later
        len_script += Opcode::LEN;

        VM_TX_MEMORY + Transaction::script_offset() + padded_len_usize(len_script)
    }

    /// Execute the transaction in a state-modifying manner.
    pub async fn call(self, fuel_client: &FuelClient) -> Result<Vec<Receipt>, Error> {
        let tx_id = fuel_client.submit(&self.tx).await?.0.to_string();
        let receipts = fuel_client.receipts(&tx_id).await?;
        let status = fuel_client.transaction_status(&tx_id).await?;
        match status {
            TransactionStatus::Failure { reason, .. } => {
                Err(Error::ContractCallError(reason, receipts))
            }
            _ => Ok(receipts),
        }
    }

    /// Execute the transaction in a simulated manner, not modifying blockchain state
    pub async fn simulate(self, fuel_client: &FuelClient) -> Result<Vec<Receipt>, Error> {
        let receipts = fuel_client.dry_run(&self.tx).await?;
        Ok(receipts)
    }
}
