use crate::fungible_token::*;
use crate::parameters::*;
use crate::sdk;
use crate::types::*;

use crate::deposit_event::*;
use crate::json::{parse_json, FAILED_PARSE};
use crate::prelude::{Address, U256};
use crate::prover::validate_eth_address;
#[cfg(feature = "log")]
use alloc::format;
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use borsh::{BorshDeserialize, BorshSerialize};

pub const CONTRACT_NAME_KEY: &str = "EthConnector";
pub const CONTRACT_FT_KEY: &str = "EthConnector.ft";
pub const NO_DEPOSIT: Balance = 0;
const GAS_FOR_FINISH_DEPOSIT: Gas = 10_000_000_000_000;
const GAS_FOR_VERIFY_LOG_ENTRY: Gas = 40_000_000_000_000;

#[derive(BorshSerialize, BorshDeserialize)]
pub struct EthConnectorContract {
    contract: EthConnector,
    ft: FungibleToken,
}

/// eth-connector specific data
#[derive(BorshSerialize, BorshDeserialize)]
pub struct EthConnector {
    pub prover_account: AccountId,
    pub eth_custodian_address: EthAddress,
}

/// Token message data
#[derive(BorshSerialize, BorshDeserialize)]
pub enum TokenMessageData {
    Near(AccountId),
    Eth {
        contract: String,
        address: EthAddress,
    },
}

impl EthConnectorContract {
    pub fn new() -> Self {
        Self {
            contract: sdk::get_contract_data(CONTRACT_NAME_KEY),
            ft: sdk::get_contract_data(CONTRACT_FT_KEY),
        }
    }

    /// Init eth-connector contract specific data
    pub fn init_contract() {
        // Check is it already initialized
        assert!(
            !sdk::storage_has_key(CONTRACT_NAME_KEY.as_bytes()),
            "ERR_CONTRACT_INITIALIZED"
        );
        #[cfg(feature = "log")]
        sdk::log("[init contract]".into());
        // Get initial contract arguments
        let args = InitCallArgs::try_from_slice(&sdk::read_input()[..]).expect(ERR_FAILED_PARSE);
        let current_account_id = sdk::current_account_id();
        let owner_id = String::from_utf8(current_account_id).unwrap();
        let mut ft = FungibleToken::new();
        // Register FT account for current contract
        ft.internal_register_account(&owner_id);
        let contract_data = EthConnector {
            prover_account: args.prover_account,
            eth_custodian_address: validate_eth_address(args.eth_custodian_address),
        };
        Self {
            contract: contract_data,
            ft,
        }
        .save_contract();
    }

    /// Parse event message data for tokens
    fn parse_event_message(&self, message: &str) -> TokenMessageData {
        let data: Vec<_> = message.split(':').collect();
        assert!(data.len() < 3);
        if data.len() == 1 {
            TokenMessageData::Near(data[0].into())
        } else {
            TokenMessageData::Eth {
                contract: data[0].into(),
                address: validate_eth_address(data[1].into()),
            }
        }
    }

    /// Deposit all types of tokens
    pub fn deposit(&self) {
        #[cfg(feature = "log")]
        sdk::log("[Deposit tokens]".into());

        // Get incoming deposit arguments
        let deposit_data: DepositCallArgs =
            DepositCallArgs::try_from_slice(&sdk::read_input()[..]).expect("ERR_FAILED_PARSE");
        let proof = deposit_data.proof;
        // Fetch event data from Proof
        let event = DepositedEvent::from_log_entry_data(&proof.log_entry_data);

        #[cfg(feature = "log")]
        sdk::log(format!(
            "Deposit started: from {} to recipient {:?} with amount: {:?} and fee {:?}",
            hex::encode(event.sender),
            event.recipient.clone(),
            event.amount.as_u128(),
            event.fee.as_u128()
        ));

        #[cfg(feature = "log")]
        if let Some(relayer_eth_account) = deposit_data.relayer_eth_account {
            sdk::log(format!(
                "Event's address {}, custodian address {}, relayer account: {}",
                hex::encode(&event.eth_custodian_address),
                hex::encode(&self.contract.eth_custodian_address),
                hex::encode(&relayer_eth_account),
            ));
        } else {
            sdk::log(format!(
                "Event's address {}, custodian address {}",
                hex::encode(&event.eth_custodian_address),
                hex::encode(&self.contract.eth_custodian_address),
            ));
        }

        assert_eq!(
            event.eth_custodian_address, self.contract.eth_custodian_address,
            "ERR_WRONG_EVENT_ADDRESS",
        );
        assert!(event.amount < event.fee, "ERR_NOT_ENOUGH_BALANCE_FOR_FEE");

        // Verify proof data with cross-cotract call at prover account
        let proof_1 = proof.try_to_vec().unwrap();
        #[cfg(feature = "log")]
        sdk::log(format!(
            "Deposit verify_log_entry for prover: {}",
            self.contract.prover_account,
        ));
        let promise0 = sdk::promise_create(
            self.contract.prover_account.as_bytes(),
            b"verify_log_entry",
            &proof_1[..],
            NO_DEPOSIT,
            GAS_FOR_VERIFY_LOG_ENTRY,
        );

        // Finilize deposit
        let promise1 = match self.parse_event_message(&event.recipient) {
            TokenMessageData::Near(account_id) => {
                let data = FinishDepositCallArgs {
                    new_owner_id: account_id,
                    amount: event.amount.as_u128(),
                    fee: event.fee.as_u128(),
                    proof,
                }
                .try_to_vec()
                .unwrap();

                sdk::promise_then(
                    promise0,
                    &sdk::current_account_id(),
                    b"finish_deposit_near",
                    &data[..],
                    NO_DEPOSIT,
                    GAS_FOR_FINISH_DEPOSIT,
                )
            }
            TokenMessageData::Eth {
                contract: _,
                address,
            } => {
                let relayer_eth_account = deposit_data
                    .relayer_eth_account
                    .expect("ERR_RELAYER_NOT_SET");
                let data = FinishDepositEthCallArgs {
                    new_owner_id: address,
                    amount: event.amount.as_u128(),
                    fee: event.fee.as_u128(),
                    relayer_eth_account,
                    proof,
                }
                .try_to_vec()
                .unwrap();

                sdk::promise_then(
                    promise0,
                    &sdk::current_account_id(),
                    b"finish_deposit_eth",
                    &data[..],
                    NO_DEPOSIT,
                    GAS_FOR_FINISH_DEPOSIT,
                )
            }
        };

        sdk::promise_return(promise1);
    }

    /// Finish deposit for NEAR accounts
    pub fn finish_deposit_near(&mut self) {
        sdk::assert_private_call();
        let data = FinishDepositCallArgs::try_from_slice(&sdk::read_input()).unwrap();
        #[cfg(feature = "log")]
        sdk::log(format!("Finish deposit NEAR amount: {}", data.amount));
        assert_eq!(sdk::promise_results_count(), 1);

        // Check promise results
        let data0: Vec<u8> = match sdk::promise_result(0) {
            PromiseResult::Successful(x) => x,
            _ => sdk::panic_utf8(b"ERR_PROMISE_INDEX"),
        };
        #[cfg(feature = "log")]
        sdk::log("Check verification_success".into());
        let verification_success: bool = bool::try_from_slice(&data0).unwrap();
        assert!(verification_success, "ERR_VERIFY_PROOF");
        self.record_proof(data.proof.get_key());

        // Mint tokens to recipient minus fee
        self.mint_near(data.new_owner_id, data.amount - data.fee);
        // Mint fee for Predecessor
        let predecessor_account_id = String::from_utf8(sdk::predecessor_account_id()).unwrap();
        self.mint_near(predecessor_account_id, data.fee);
        // Save new contract data
        self.save_contract();
    }

    /// Finish deposit for ETH accounts
    pub fn finish_deposit_eth(&mut self) {
        sdk::assert_private_call();
        let data = FinishDepositEthCallArgs::try_from_slice(&sdk::read_input()).unwrap();
        #[cfg(feature = "log")]
        sdk::log(format!("Finish deposit ETH amount: {}", data.amount));
        assert_eq!(sdk::promise_results_count(), 1);

        // Check promise results
        let data0: Vec<u8> = match sdk::promise_result(0) {
            PromiseResult::Successful(x) => x,
            _ => sdk::panic_utf8(b"ERR_PROMISE_INDEX"),
        };
        #[cfg(feature = "log")]
        sdk::log("Check verification_success".into());
        let verification_success: bool = bool::try_from_slice(&data0).unwrap();
        assert!(verification_success, "ERR_VERIFY_PROOF");
        self.record_proof(data.proof.get_key());

        // Mint tokens to recipient minus fee
        self.mint_eth(data.new_owner_id, data.amount - data.fee);
        // Mint tokens fee to Relayer
        #[cfg(feature = "log")]
        sdk::log(format!(
            "relayer_eth_account: {}",
            hex::encode(data.relayer_eth_account)
        ));
        self.mint_eth(data.relayer_eth_account, data.fee);
        // Save new contract data
        self.save_contract();
    }

    /// Internal ETH deposit logic
    pub(crate) fn internal_deposit_eth(&mut self, address: &Address, amount: &U256) {
        self.ft.internal_deposit_eth(address.0, amount.as_u128());
        self.save_contract();
    }

    /// Internal ETH withdraw ETH logic
    pub(crate) fn internal_remove_eth(&mut self, address: &Address, amount: &U256) {
        self.ft.internal_withdraw_eth(address.0, amount.as_u128());
        self.save_contract();
    }

    /// Record used proof as hash key
    fn record_proof(&mut self, key: String) {
        #[cfg(feature = "log")]
        sdk::log("Record proof".into());
        let key = key.as_str();

        assert!(!self.check_used_event(key), "ERR_PROOF_EXIST");
        self.save_used_event(key);
    }

    ///  Mint NEAR tokens
    fn mint_near(&mut self, owner_id: AccountId, amount: Balance) {
        #[cfg(feature = "log")]
        sdk::log(format!("Mint NEAR {} tokens for: {}", amount, owner_id));

        if self.ft.accounts_get(&owner_id).is_none() {
            self.ft.accounts_insert(&owner_id, 0);
        }
        self.ft.internal_deposit(&owner_id, amount);
        #[cfg(feature = "log")]
        sdk::log("Mint NEAR success".into());
    }

    ///  Mint ETH tokens
    fn mint_eth(&mut self, owner_id: EthAddress, amount: Balance) {
        #[cfg(feature = "log")]
        sdk::log(format!(
            "Mint ETH {} tokens for: {}",
            amount,
            hex::encode(owner_id)
        ));
        self.ft.internal_deposit_eth(owner_id, amount);
        #[cfg(feature = "log")]
        sdk::log("Mint ETH success".into());
    }

    /// Burn NEAR tokens
    fn burn_near(&mut self, owner_id: AccountId, amount: Balance) {
        #[cfg(feature = "log")]
        sdk::log(format!("Burn NEAR {} tokens for: {}", amount, owner_id));
        self.ft.internal_withdraw(&owner_id, amount);
    }

    /// Burn ETH tokens
    fn burn_eth(&mut self, address: EthAddress, amount: Balance) {
        #[cfg(feature = "log")]
        sdk::log(format!(
            "Burn ETH {} tokens for: {}",
            amount,
            hex::encode(address)
        ));
        self.ft.internal_withdraw_eth(address, amount);
    }

    pub fn withdraw_near(&mut self) {
        #[cfg(feature = "log")]
        sdk::log("Start withdraw NEAR".into());
        let args: WithdrawCallArgs =
            WithdrawCallArgs::from(parse_json(&sdk::read_input()).expect_utf8(FAILED_PARSE));
        let recipient_address = validate_eth_address(args.recipient_id);
        let res = WithdrawResult {
            recipient_id: recipient_address,
            amount: args.amount,
            eth_custodian_address: self.contract.eth_custodian_address,
        }
        .try_to_vec()
        .unwrap();
        // Burn tokens to recipient
        let predecessor_account_id = String::from_utf8(sdk::predecessor_account_id()).unwrap();
        self.burn_near(predecessor_account_id, args.amount);
        // Save new contract data
        self.save_contract();
        sdk::return_output(&res[..]);
    }

    /// Withdraw ETH tokens
    pub fn withdraw_eth(&mut self) {
        use crate::prover;
        #[cfg(feature = "log")]
        sdk::log("Start withdraw ETH".into());

        let args: WithdrawEthCallArgs =
            WithdrawEthCallArgs::from(parse_json(&sdk::read_input()).expect_utf8(FAILED_PARSE));
        assert!(
            prover::verify_withdraw_eip712(
                args.sender,
                args.eth_recipient,
                self.contract.eth_custodian_address,
                args.amount,
                args.eip712_signature
            ),
            "ERR_WRONG_EIP712_MSG"
        );
        let res = WithdrawResult {
            recipient_id: args.eth_recipient,
            amount: args.amount.as_u128(),
            eth_custodian_address: self.contract.eth_custodian_address,
        }
        .try_to_vec()
        .unwrap();
        // Burn tokens to recipient
        self.burn_eth(args.eth_recipient, args.amount.as_u128());
        // Save new contract data
        self.save_contract();
        sdk::return_output(&res[..]);
    }

    /// Return total supply of NEAR + ETH
    pub fn ft_total_supply(&self) {
        let total_supply = self.ft.ft_total_supply();
        sdk::return_output(&total_supply.to_string().as_bytes());
        #[cfg(feature = "log")]
        sdk::log(format!("Total supply: {}", total_supply));
    }

    /// Return total supply of NEAR
    pub fn ft_total_supply_near(&self) {
        let total_supply = self.ft.ft_total_supply_near();
        sdk::return_output(&total_supply.to_string().as_bytes());
        #[cfg(feature = "log")]
        sdk::log(format!("Total supply NEAR: {}", total_supply));
    }

    /// Return total supply of ETH
    pub fn ft_total_supply_eth(&self) {
        let total_supply = self.ft.ft_total_supply_eth();
        sdk::return_output(&total_supply.to_string().as_bytes());
        #[cfg(feature = "log")]
        sdk::log(format!("Total supply ETH: {}", total_supply));
    }

    /// Return balance of NEAR
    pub fn ft_balance_of(&self) {
        let args =
            BalanceOfCallArgs::from(parse_json(&sdk::read_input()).expect_utf8(FAILED_PARSE));
        let balance = self.ft.ft_balance_of(&args.account_id);
        sdk::return_output(&balance.to_string().as_bytes());
        #[cfg(feature = "log")]
        sdk::log(format!(
            "Balance of NEAR [{}]: {}",
            args.account_id, balance
        ));
    }

    /// Return balance of ETH
    pub fn ft_balance_of_eth(&self) {
        let args =
            BalanceOfEthCallArgs::from(parse_json(&sdk::read_input()).expect_utf8(FAILED_PARSE));
        let balance = self.ft.internal_unwrap_balance_of_eth(args.address);
        #[cfg(feature = "log")]
        sdk::log(format!(
            "Balance of ETH [{}]: {}",
            hex::encode(args.address),
            balance
        ));
        sdk::return_output(&balance.to_string().as_bytes());
    }

    /// Transfer between NEAR accounts
    pub fn ft_transfer(&mut self) {
        let args: TransferCallArgs =
            TransferCallArgs::from(parse_json(&sdk::read_input()).expect_utf8(FAILED_PARSE));

        self.ft
            .ft_transfer(&args.receiver_id, args.amount, &args.memo);
        self.save_contract();
        #[cfg(feature = "log")]
        sdk::log(format!(
            "Transfer amount {} to {} success with memo: {:?}",
            args.amount, args.receiver_id, args.memo
        ));
    }

    /// FT resolve transfer logic
    pub fn ft_resolve_transfer(&mut self) {
        sdk::assert_private_call();
        let args: ResolveTransferCallArgs =
            ResolveTransferCallArgs::try_from_slice(&sdk::read_input()).unwrap();
        let amount = self
            .ft
            .ft_resolve_transfer(&args.sender_id, &args.receiver_id, args.amount);
        // `ft_resolve_transfer` can changed `total_supply` so we should save contract
        self.save_contract();
        sdk::return_output(&amount.to_string().as_bytes());
        #[cfg(feature = "log")]
        sdk::log(format!(
            "Resolve transfer of {} from {} to {} success",
            args.amount, args.sender_id, args.receiver_id
        ));
    }

    /// FT transfer call from sender account (invoker account) to receiver
    pub fn ft_transfer_call(&mut self) {
        let args =
            TransferCallCallArgs::try_from_slice(&sdk::read_input()).expect(ERR_FAILED_PARSE);
        #[cfg(feature = "log")]
        sdk::log(format!(
            "Transfer call to {} amount {}",
            args.receiver_id, args.amount,
        ));

        self.ft
            .ft_transfer_call(&args.receiver_id, args.amount, &args.memo, args.msg);
    }

    /// FT storage deposit logic
    pub fn storage_deposit(&mut self) {
        let args: StorageDepositCallArgs =
            StorageDepositCallArgs::from(parse_json(&sdk::read_input()).expect_utf8(FAILED_PARSE));
        let res = self
            .ft
            .storage_deposit(args.account_id.as_ref(), args.registration_only)
            .try_to_vec()
            .unwrap();
        self.save_contract();
        sdk::return_output(&res[..]);
    }

    /// FT storage withdraw
    pub fn storage_withdraw(&mut self) {
        let args: StorageWithdrawCallArgs =
            StorageWithdrawCallArgs::from(parse_json(&sdk::read_input()).expect_utf8(FAILED_PARSE));
        let res = self.ft.storage_withdraw(args.amount).try_to_vec().unwrap();
        self.save_contract();
        sdk::return_output(&res[..]);
    }

    /// Get balance of storage
    pub fn storage_balance_of(&self) {
        let args: StorageBalanceOfCallArgs = StorageBalanceOfCallArgs::from(
            parse_json(&sdk::read_input()).expect_utf8(FAILED_PARSE),
        );
        let res = self
            .ft
            .storage_balance_of(&args.account_id)
            .try_to_vec()
            .unwrap();
        sdk::return_output(&res[..]);
    }

    /// Save eth-connecor contract data
    fn save_contract(&mut self) {
        sdk::save_contract(CONTRACT_NAME_KEY.as_bytes(), &self.contract);
        sdk::save_contract(CONTRACT_FT_KEY.as_bytes(), &self.ft);
    }

    /// Generate key for used events from Prood
    fn used_event_key(&self, key: &str) -> String {
        [CONTRACT_NAME_KEY, "used-event", key].join(".")
    }

    /// Save already used event proof as hash key
    fn save_used_event(&self, key: &str) {
        sdk::save_contract(&self.used_event_key(key).as_bytes(), &0u8);
    }

    /// Check is event of proof already used
    fn check_used_event(&self, key: &str) -> bool {
        sdk::storage_has_key(&self.used_event_key(key).as_bytes())
    }
}
