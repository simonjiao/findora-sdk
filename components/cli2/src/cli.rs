#![feature(in_band_lifetimes)]
#![deny(warnings)]
#![allow(clippy::type_complexity)]
use ledger::data_model::*;
use promptly::prompt_default;
use serde::{Deserialize, Serialize};
use snafu::{Backtrace, GenerateBacktrace, OptionExt, ResultExt, Snafu};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use structopt::StructOpt;

use submission_server::{TxnHandle, TxnStatus};
use txn_builder::TransactionBuilder;
use utils::{HashOf, SignatureOf};
use zei::xfr::sig::{XfrKeyPair, XfrPublicKey};
use zei::xfr::structs::{OpenAssetRecord, OwnerMemo};

pub mod actions;
pub mod helpers;
pub mod kv;

use crate::actions::*;
use kv::{HasEncryptedTable, HasTable, KVError, KVStore};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerStateCommitment(pub  (HashOf<Option<StateCommitmentData>>,
                                       u64,
                                       SignatureOf<(HashOf<Option<StateCommitmentData>>, u64)>));

pub struct FreshNamer {
  base: String,
  i: u64,
  delim: String,
}

impl FreshNamer {
  pub fn new(base: String, delim: String) -> Self {
    Self { base, i: 0, delim }
  }
}

impl Iterator for FreshNamer {
  type Item = String;
  fn next(&mut self) -> Option<String> {
    let ret = if self.i == 0 {
      self.base.clone()
    } else {
      format!("{}{}{}", self.base, self.delim, self.i - 1)
    };
    self.i += 1;
    Some(ret)
  }
}

fn default_sub_server() -> String {
  "https://testnet.findora.org:8669".to_string()
}

fn default_ledger_server() -> String {
  "https://testnet.findora.org:8668".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CliConfig {
  #[serde(default = "default_sub_server")]
  pub submission_server: String,
  #[serde(default = "default_ledger_server")]
  pub ledger_server: String,
  pub open_count: u64,
  #[serde(default)]
  pub ledger_sig_key: Option<XfrPublicKey>,
  #[serde(default)]
  pub ledger_state: Option<LedgerStateCommitment>,
  #[serde(default)]
  pub active_txn: Option<TxnBuilderName>,
}

impl HasTable for CliConfig {
  const TABLE_NAME: &'static str = "config";
  type Key = String;
}

#[derive(Ord, PartialOrd, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash, Default)]
pub struct AssetTypeName(pub String);

impl HasTable for AssetTypeEntry {
  const TABLE_NAME: &'static str = "asset_types";
  type Key = AssetTypeName;
}

#[derive(Ord, PartialOrd, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash, Default)]
pub struct KeypairName(pub String);

// impl HasTable for XfrKeyPair {
//   const TABLE_NAME: &'static str = "key_pairs";
//   type Key = KeypairName;
// }

// TODO(Nathan M): I was unable to find a method in zei for recombining key pairs,
// so this sort of doesn't take really advantage of the backend stuff mixed
// plaintext/cleartext for now
impl HasEncryptedTable for XfrKeyPair {
  const TABLE_NAME: &'static str = "enc_key_pairs";
  type Key = KeypairName;
  type Clear = XfrPublicKey;
}

#[derive(Ord, PartialOrd, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash, Default)]
pub struct PubkeyName(pub String);

impl HasTable for XfrPublicKey {
  const TABLE_NAME: &'static str = "public_keys";
  type Key = PubkeyName;
}

#[derive(Ord, PartialOrd, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash, Default)]
pub struct TxnName(pub String);

impl HasTable for (Transaction, TxnMetadata) {
  const TABLE_NAME: &'static str = "transactions";
  type Key = TxnName;
}

#[derive(Ord, PartialOrd, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash, Default)]
pub struct TxnBuilderName(pub String);

impl HasTable for TxnBuilderEntry {
  const TABLE_NAME: &'static str = "transaction_builders";
  type Key = TxnBuilderName;
}

#[derive(Ord, PartialOrd, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash, Default)]
pub struct TxoName(pub String);

impl HasTable for TxoCacheEntry {
  const TABLE_NAME: &'static str = "txo_cache";
  type Key = TxoName;
}

#[derive(Snafu, Debug)]
pub enum CliError {
  #[snafu(context(false))]
  KV {
    #[snafu(backtrace)]
    source: KVError,
  },
  #[snafu(context(false))]
  #[snafu(display("Error performing HTTP request"))]
  Reqwest {
    source: reqwest::Error,
    backtrace: Backtrace,
  },
  #[snafu(context(false))]
  #[snafu(display("Error during (de)serialization"))]
  Serialization {
    source: serde_json::error::Error,
    backtrace: Backtrace,
  },
  #[snafu(context(false))]
  #[snafu(display("Error reading user input"))]
  RustyLine {
    source: rustyline::error::ReadlineError,
    backtrace: Backtrace,
  },
  #[snafu(display("Error creating user directory or file at {}", file.display()))]
  UserFile {
    source: std::io::Error,
    file: std::path::PathBuf,
    backtrace: Backtrace,
  },
  #[snafu(display("Failed to locate user's home directory"))]
  HomeDir,
  #[snafu(display("Failed to fetch new public key from server"))]
  NewPublicKeyFetch {
    source: structopt::clap::Error,
    backtrace: Backtrace,
  },
  #[snafu(display("Failed to read password"))]
  Password {
    #[snafu(backtrace)]
    source: helpers::PasswordReadError,
  },

  #[snafu(display("Misc"))] // TODO remove that with something more informative
  Misc,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct TxnMetadata {
  handle: Option<TxnHandle>,
  status: Option<TxnStatus>,
  new_asset_types: BTreeMap<AssetTypeName, AssetTypeEntry>,
  #[serde(default)]
  operations: Vec<OpMetadata>,
  #[serde(default)]
  signers: Vec<KeypairName>,
  // TODO
  #[serde(default)]
  new_txos: Vec<(String, TxoCacheEntry)>,
  #[serde(default)]
  finalized_txos: Option<Vec<TxoName>>,
  #[serde(default)]
  spent_txos: Vec<TxoName>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxoCacheEntry {
  sid: Option<TxoSID>,
  owner: Option<PubkeyName>,
  #[serde(default)]
  asset_type: Option<AssetTypeName>,
  // What has this Txo been authenticated against?
  ledger_state: Option<LedgerStateCommitment>,
  record: TxOutput,
  owner_memo: Option<OwnerMemo>,
  opened_record: Option<OpenAssetRecord>,
  unspent: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetTypeEntry {
  asset: Asset,
  issuer_nick: Option<PubkeyName>,
  issue_seq_num: u64,
}

fn indent_of(indent_level: u64) -> String {
  let mut ret: String = Default::default();
  for _ in 0..indent_level {
    ret = format!("{}{}", ret, " ");
  }
  ret
}

#[allow(clippy::enum_variant_names)]
#[derive(Clone, Debug, Serialize, Deserialize)]
enum OpMetadata {
  DefineAsset {
    issuer_nick: PubkeyName,
    asset_nick: AssetTypeName,
  },
  IssueAsset {
    issuer_nick: PubkeyName,
    asset_nick: AssetTypeName,
    output_name: String,
    output_amt: u64,
    issue_seq_num: u64,
  },
  TransferAssets {
    inputs: Vec<(String, TxoCacheEntry)>,
    outputs: Vec<(String, TxoCacheEntry)>,
  },
}

fn display_op_metadata(indent_level: u64, ent: &OpMetadata) {
  let ind = indent_of(indent_level);
  match ent {
    OpMetadata::DefineAsset { asset_nick,
                              issuer_nick, } => {
      println!("{}DefineAsset `{}`", ind, asset_nick.0);
      println!("{} issued by `{}`", ind, issuer_nick.0);
    }
    OpMetadata::IssueAsset { issuer_nick,
                             asset_nick,
                             output_name,
                             output_amt,
                             issue_seq_num, } => {
      println!("{}IssueAsset {} of `{}`", ind, output_amt, asset_nick.0);
      println!("{} issued to `{}` as issuance #{} named `{}`",
               ind, issuer_nick.0, issue_seq_num, output_name);
    }
    OpMetadata::TransferAssets { inputs, outputs } => {
      println!("{}TransferAssets:", ind);
      println!("{} Inputs:", ind);
      display_txos(indent_level + 1, &inputs, &None);
      println!("{} Outputs:", ind);
      display_txos(indent_level + 1, &outputs, &None);
    }
  }
}

fn display_asset_type_defs(indent_level: u64, ent: &BTreeMap<AssetTypeName, AssetTypeEntry>) {
  let ind = indent_of(indent_level);
  for (nick, asset_ent) in ent.iter() {
    println!("{}{}:", ind, nick.0);
    display_asset_type(indent_level + 1, asset_ent);
  }
}

fn display_operations(indent_level: u64, operations: &[OpMetadata]) {
  for op in operations.iter() {
    display_op_metadata(indent_level, op);
  }
}

fn display_txo_entry(indent_level: u64, txo: &TxoCacheEntry) {
  let ind = indent_of(indent_level);
  println!("{}sid: {}", ind, serialize_or_str(&txo.sid, "<UNKNOWN>"));
  println!("{}Owned by: {}{}",
           ind,
           serde_json::to_string(&txo.record.0.public_key).unwrap(),
           if let Some(o) = txo.owner.as_ref() {
             format!(" ({})", o.0)
           } else {
             "".to_string()
           });
  println!("{}Record Type: {}",
           ind,
           serde_json::to_string(&txo.record.0.get_record_type()).unwrap());
  println!("{}Amount: {}",
           ind,
           txo.record
              .0
              .amount
              .get_amount()
              .map(|x| format!("{}", x))
              .unwrap_or_else(|| "<SECRET>".to_string()));
  println!("{}Type: {}",
           ind,
           txo.record
              .0
              .asset_type
              .get_asset_type()
              .map(|x| AssetTypeCode { val: x }.to_base64())
              .unwrap_or_else(|| "<SECRET>".to_string()));
  if let Some(open_ar) = txo.opened_record.as_ref() {
    println!("{}Decrypted Amount: {}", ind, open_ar.amount);
    println!("{}Decrypted Type: {}",
             ind,
             AssetTypeCode { val: open_ar.asset_type }.to_base64());
  }
  println!("{}Spent? {}",
           ind,
           if txo.unspent { "Unspent" } else { "Spent" });
  println!("{}Have owner memo? {}",
           ind,
           if txo.owner_memo.is_some() {
             "Yes"
           } else {
             "No"
           });
}

fn display_txos(indent_level: u64,
                txos: &[(String, TxoCacheEntry)],
                finalized_names: &Option<Vec<TxoName>>) {
  let ind = indent_of(indent_level);
  for (i, (nick, txo)) in txos.iter().enumerate() {
    let fin = finalized_names.as_ref()
                             .and_then(|x| x.get(i))
                             .map(|x| x.0.to_string())
                             .unwrap_or_else(|| "Not finalized".to_string());
    println!("{}{} ({}):", ind, nick, fin);
    display_txo_entry(indent_level + 1, txo);
  }
}

fn display_txn_builder(indent_level: u64, ent: &TxnBuilderEntry) {
  let ind = indent_of(indent_level);
  println!("{}Operations:", ind);
  display_operations(indent_level + 1, &ent.operations);

  println!("{}New asset types defined:", ind);
  display_asset_type_defs(indent_level + 1, &ent.new_asset_types);

  println!("{}New asset records:", ind);
  display_txos(indent_level + 1, &ent.new_txos, &None);

  println!("{}Signers:", ind);
  for nick in ent.signers.iter() {
    println!("{} - `{}`", ind, nick.0);
  }

  print!("{}Consuming TXOs:", ind);
  for n in ent.spent_txos.iter() {
    print!(" {}", n.0);
  }
  println!();
}

fn display_txn(indent_level: u64, ent: &(Transaction, TxnMetadata)) {
  let ind = indent_of(indent_level);
  println!("{}seq_id: {}", ind, ent.0.seq_id);
  println!("{}Handle: {}",
           ind,
           serialize_or_str(&ent.1.handle, "<UNKNOWN>"));
  println!("{}Status: {}",
           ind,
           serialize_or_str(&ent.1.status, "<UNKNOWN>"));
  println!("{}Operations:", ind);
  display_operations(indent_level + 1, &ent.1.operations);

  println!("{}New asset types defined:", ind);
  for (nick, asset_ent) in ent.1.new_asset_types.iter() {
    println!("{} {}:", ind, nick.0);
    display_asset_type(indent_level + 2, asset_ent);
  }

  println!("{}New asset records:", ind);
  display_txos(indent_level + 1, &ent.1.new_txos, &ent.1.finalized_txos);

  println!("{}Signers:", ind);
  for nick in ent.1.signers.iter() {
    println!("{} - `{}`", ind, nick.0);
  }

  print!("{}Consuming TXOs:", ind);
  for n in ent.1.spent_txos.iter() {
    print!(" {}", n.0);
  }
  println!();
}

fn display_asset_type(indent_level: u64, ent: &AssetTypeEntry) {
  let ind = indent_of(indent_level);
  println!("{}issuer nickname: {}",
           ind,
           ent.issuer_nick
              .as_ref()
              .map(|x| x.0.clone())
              .unwrap_or_else(|| "<UNKNOWN>".to_string()));
  println!("{}issuer public key: {}",
           ind,
           serde_json::to_string(&ent.asset.issuer.key).unwrap());
  println!("{}code: {}", ind, ent.asset.code.to_base64());
  println!("{}memo: `{}`", ind, ent.asset.memo.0);
  println!("{}issue_seq_number: {}", ind, ent.issue_seq_num);
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxnBuilderEntry {
  builder: TransactionBuilder,
  #[serde(default)]
  operations: Vec<OpMetadata>,
  #[serde(default)]
  new_asset_types: BTreeMap<AssetTypeName, AssetTypeEntry>,
  #[serde(default)]
  signers: Vec<KeypairName>,
  // TODO
  #[serde(default)]
  new_txos: Vec<(String, TxoCacheEntry)>,
  #[serde(default)]
  spent_txos: Vec<TxoName>,
}

pub trait CliDataStore {
  fn get_config(&self) -> Result<CliConfig, CliError>;
  fn update_config<F: FnOnce(&mut CliConfig) -> Result<(), CliError>>(&mut self,
                                                                      f: F)
                                                                      -> Result<(), CliError>;

  fn get_keypairs(&self) -> Result<Vec<KeypairName>, CliError>;
  fn get_keypair_pubkey(&self, k: &KeypairName) -> Result<Option<XfrPublicKey>, CliError>;
  fn delete_keypair(&mut self, k: &KeypairName) -> Result<(), CliError>;
  fn with_keypair<E: std::error::Error + 'static, F: FnOnce(Option<&XfrKeyPair>) -> Result<(), E>>(
    &mut self,
    k: &KeypairName,
    f: F)
    -> Result<(), CliError>;
  fn get_pubkeys(&self) -> Result<BTreeMap<PubkeyName, XfrPublicKey>, CliError>;
  fn get_pubkey(&self, k: &PubkeyName) -> Result<Option<XfrPublicKey>, CliError>;
  fn delete_pubkey(&mut self, k: &PubkeyName) -> Result<Option<XfrPublicKey>, CliError>;
  fn add_key_pair(&mut self, k: &KeypairName, kp: XfrKeyPair) -> Result<(), CliError>;
  fn add_public_key(&mut self, k: &PubkeyName, pk: XfrPublicKey) -> Result<(), CliError>;

  fn get_built_transactions(&self)
                            -> Result<BTreeMap<TxnName, (Transaction, TxnMetadata)>, CliError>;
  fn get_built_transaction(&self,
                           k: &TxnName)
                           -> Result<Option<(Transaction, TxnMetadata)>, CliError>;
  fn build_transaction(&mut self,
                       k_orig: &TxnBuilderName,
                       k_new: &TxnName,
                       metadata: TxnMetadata)
                       -> Result<(Transaction, TxnMetadata), CliError>;
  fn update_txn_metadata<E: std::error::Error + 'static,
                           F: FnOnce(&mut TxnMetadata) -> Result<(), E>>(
    &mut self,
    k: &TxnName,
    f: F)
    -> Result<(), CliError>;

  fn prepare_transaction(&mut self, k: &TxnBuilderName, seq_id: u64) -> Result<(), CliError>;
  fn get_txn_builder(&self, k: &TxnBuilderName) -> Result<Option<TxnBuilderEntry>, CliError>;
  fn get_txn_builders(&self) -> Result<BTreeMap<TxnBuilderName, TxnBuilderEntry>, CliError>;
  fn with_txn_builder<E: std::error::Error + 'static,
                        F: FnOnce(&mut TxnBuilderEntry) -> Result<(), E>>(
    &mut self,
    k: &TxnBuilderName,
    f: F)
    -> Result<(), CliError>;

  fn get_cached_txos(&self) -> Result<BTreeMap<TxoName, TxoCacheEntry>, CliError>;
  fn get_cached_txo(&self, k: &TxoName) -> Result<Option<TxoCacheEntry>, CliError>;
  fn delete_cached_txo(&mut self, k: &TxoName) -> Result<(), CliError>;
  fn cache_txo(&mut self, k: &TxoName, ent: TxoCacheEntry) -> Result<(), CliError>;

  fn get_asset_types(&self) -> Result<BTreeMap<AssetTypeName, AssetTypeEntry>, CliError>;
  fn get_asset_type(&self, k: &AssetTypeName) -> Result<Option<AssetTypeEntry>, CliError>;
  fn update_asset_type<E: std::error::Error + 'static,
                         F: FnOnce(&mut AssetTypeEntry) -> Result<(), E>>(
    &mut self,
    k: &AssetTypeName,
    f: F)
    -> Result<(), CliError>;
  fn delete_asset_type(&self, k: &AssetTypeName) -> Result<Option<AssetTypeEntry>, CliError>;
  fn add_asset_type(&self, k: &AssetTypeName, ent: AssetTypeEntry) -> Result<(), CliError>;
}

fn prompt_for_config(prev_conf: Option<CliConfig>) -> Result<CliConfig, CliError> {
  let default_sub_server = prev_conf.as_ref()
                                    .map(|x| x.submission_server.clone())
                                    .unwrap_or_else(default_sub_server);
  let default_ledger_server = prev_conf.as_ref()
                                       .map(|x| x.ledger_server.clone())
                                       .unwrap_or_else(default_ledger_server);
  Ok(CliConfig { submission_server: prompt_default("Submission Server?", default_sub_server)?,
                 ledger_server: prompt_default("Ledger Access Server?", default_ledger_server)?,
                 open_count: 0,
                 ledger_sig_key: prev_conf.as_ref().and_then(|x| x.ledger_sig_key),
                 ledger_state: prev_conf.as_ref().and_then(|x| x.ledger_state.clone()),
                 active_txn: prev_conf.as_ref().and_then(|x| x.active_txn.clone()) })
}

#[derive(StructOpt, Debug)]
#[structopt(about = "Build and manage transactions and assets on a findora ledger",
            rename_all = "kebab-case")]
enum Actions {
  //////////////////// Simple API  /////////////////////////////////////////////////////////////////
  /// Initialize or change your local database configuration
  Setup {},

  /// Generate bash/zsh/fish/powershell completion files for this CLI
  GenCompletions {
    /// Output directory
    #[structopt(parse(from_os_str))]
    outdir: PathBuf,
    /// bash
    #[structopt(long)]
    bash: bool,
    /// zsh
    #[structopt(long)]
    zsh: bool,
    /// fish
    #[structopt(long)]
    /// pow
    fish: bool,
    /// poweshell
    #[structopt(long)]
    powershell: bool,
    /// elvish
    #[structopt(long)]
    elvish: bool,
  },

  /// Display the current configuration and ledger state
  ListConfig {},

  /// Get the latest state commitment data from the ledger
  QueryLedgerState {
    /// Whether to forget the old ledger public key
    #[structopt(short, long)]
    forget_old_key: bool,
  },

  /// Generate a new key pair for <nick>
  KeyGen {
    /// Identity nickname
    nick: String,
  },

  /// Load an existing key pair for <nick>
  LoadKeypair {
    /// Identity nickname
    nick: String,
  },

  /// Load a public key for <nick>
  LoadPublicKey {
    /// Identity nickname
    nick: String,
  },

  /// List all the key pairs present in the database.
  ListKeys {},

  /// Display information about the public key for <nick>
  ListPublicKey {
    /// Identity nickname
    nick: String,
  },

  /// Display information about the key pair for <nick>
  ListKeypair {
    /// Identity nickname
    nick: String,

    /// Also display the secret key
    #[structopt(short, long)]
    show_secret: bool,
  },

  /// Permanently delete the key pair for <nick>
  DeleteKeypair {
    /// Identity nickname
    nick: String,
  },

  /// Permanently delete the public key for <nick>
  DeletePublicKey {
    /// Identity nickname
    nick: String,
  },

  /// Define an asset in a single step
  SimpleDefineAsset {
    /// Issuer key
    issuer_nick: String,
    /// Name for the asset type
    asset_nick: String,
  },

  /// Issue an asset in a single step
  SimpleIssueAsset {
    /// Name for the asset type
    asset_nick: String,
    /// Number of coins to issue
    amount: u64,
  },

  //////////////////// Advanced API  ///////////////////////////////////////////////////////////////
  /// List all the asset types
  ListAssetTypes {},

  ListAssetType {
    /// Asset type nickname
    nick: String,
  },
  QueryAssetType {
    /// Replace the existing asset type entry (if it exists)
    #[structopt(short, long)]
    replace: bool,
    /// Asset type nickname
    nick: String,
    /// Asset type code (b64)
    code: String,
  },

  //   /// Query the asset issuance sequence number
  //   QueryAssetIssuanceNum {
  //     /// Asset type nickname
  //     nick: String,
  //   },
  /// Initialize a transaction builder
  PrepareTransaction {
    /// Optional transaction name
    #[structopt(default_value = "txn")]
    nick: String,
    /// Force the transaction's name to be <nick>, instead of the first free <nick>.<n>
    #[structopt(short, long)]
    exact: bool,
  },

  /// List the transaction builders which are in progress
  ListTxnBuilders {},

  /// List the details of a transaction builder
  ListTxnBuilder {
    /// Which builder?
    nick: String,
  },

  /// Finalize a transaction, preparing it for submission
  BuildTransaction {
    #[structopt(short, long)]
    /// Force the transaction's name to be <txn-nick>, instead of the first free <txn-nick>.<n>
    exact: bool,
    /// Which transaction builder?
    #[structopt(short, long)]
    builder: Option<String>,
    /// Name for the built transaction (defaults to <builder>)
    txn_nick: Option<String>,
  },

  /// Create the definition of an asset and put it in a transaction builder
  DefineAsset {
    #[structopt(short, long)]
    /// Which builder?
    builder: Option<String>,
    /// Issuer key
    issuer_nick: String,
    /// Name for the asset type
    asset_nick: String,
  },

  /// Create a transaction part corresponding to the issuance of an asset
  IssueAsset {
    #[structopt(short, long)]
    /// Which builder?
    builder: Option<String>,
    /// Name for the asset type
    asset_nick: String,
    /// Sequence number of this issuance
    issue_seq_num: u64,
    /// Amount to issue
    amount: u64,
  },

  /// Create a transaction part corresponding to the transfer of an asset
  TransferAssets {
    #[structopt(short, long)]
    /// Which builder?
    builder: Option<String>,
  },

  /// Show the details of a built transaction
  ListBuiltTransaction {
    /// Nickname of the transaction
    nick: String,
  },

  /// Show the details of all built transactions
  ListBuiltTransactions {
    // TODO: options?
  },

  /// Submit a built transaction
  Submit {
    /// Which txn?
    nick: String,
  },

  /// Show the status of a submitted transaction
  Status {
    // TODO: how are we indexing in-flight transactions?
    /// Which txn?
    txn: String,
  },

  // TODO doc
  StatusCheck {
    // TODO: how are we indexing in-flight transactions?
    /// Which txn?
    txn: String,
  },

  // TODO doc
  ListTxo {
    /// nickname
    id: String,
  },

  // TODO doc
  ListTxos {
    /// Only unspent?
    #[structopt(short, long)]
    unspent: bool,
  },

  // // TODO doc
  // ListOwnedUtxos {
  //   /// Whose UTXOs?
  //   id: String,
  // },

  // TODO doc
  QueryTxo {
    /// Local nickname?
    nick: String,
    /// Which SID?
    sid: Option<u64>,
  },
}

fn serialize_or_str<T: Serialize>(x: &Option<T>, s: &str) -> String {
  x.as_ref()
   .map(|x| serde_json::to_string(&x).unwrap())
   .unwrap_or_else(|| s.to_string())
}

fn print_conf(conf: &CliConfig) {
  println!("Submission server: {}", conf.submission_server);
  println!("Ledger access server: {}", conf.ledger_server);
  println!("Ledger public signing key: {}",
           serialize_or_str(&conf.ledger_sig_key, "<UNKNOWN>"));
  println!("Ledger state commitment: {}",
           conf.ledger_state
               .as_ref()
               .map(|x| b64enc(&((x.0).0).0.hash))
               .unwrap_or_else(|| "<UNKNOWN>".to_string()));
  println!("Ledger block idx: {}",
           conf.ledger_state
               .as_ref()
               .map(|x| format!("{}", (x.0).1))
               .unwrap_or_else(|| "<UNKNOWN>".to_string()));
  println!("Current focused transaction builder: {}",
           conf.active_txn
               .as_ref()
               .map(|x| x.0.clone())
               .unwrap_or_else(|| "<NONE>".to_string()));
}

fn run_action<S: CliDataStore>(action: Actions, store: &mut S) -> Result<(), CliError> {
  // println!("{:?}", action);

  use Actions::*;
  let ret = match action {
    //////////////////// Simple API  ///////////////////////////////////////////////////////////////
    Setup {} => setup(store),

    GenCompletions { .. } => panic!("GenCompletions should've been handle already!"),

    ListConfig {} => list_config(store),

    KeyGen { nick } => key_gen(store, nick),

    ListKeys {} => list_keys(store),

    ListKeypair { nick, show_secret } => list_keypair(store, nick, show_secret),

    ListPublicKey { nick } => list_public_key(store, nick),

    LoadKeypair { nick } => load_key_pair(store, nick),

    LoadPublicKey { nick } => load_public_key(store, nick),

    DeleteKeypair { nick } => delete_keypair(store, nick),

    DeletePublicKey { nick } => delete_public_key(store, nick),

    SimpleDefineAsset { issuer_nick,
                        asset_nick, } => simple_define_asset(store, issuer_nick, asset_nick),

    SimpleIssueAsset { asset_nick, amount } => simple_issue_asset(store, asset_nick, amount),

    //////////////////// Advanced API  /////////////////////////////////////////////////////////////
    QueryLedgerState { forget_old_key } => query_ledger_state(store, forget_old_key),

    ListTxos { unspent } => list_txos(store, unspent),

    ListTxo { id } => list_txo(store, id),

    QueryTxo { nick, sid } => query_txo(store, nick, sid),

    ListTxnBuilders {} => list_txn_builders(store),

    ListTxnBuilder { nick } => list_txn_builder(store, nick),

    ListAssetTypes {} => list_asset_types(store),

    ListAssetType { nick } => list_asset_type(store, nick),

    QueryAssetType { replace,
                     nick,
                     code, } => query_asset_type(store, replace, nick, code),

    PrepareTransaction { nick, exact } => prepare_transaction(store, nick, exact),

    ListBuiltTransaction { nick } => list_built_transaction(store, nick),

    ListBuiltTransactions {} => list_built_transactions(store),

    Status { txn } => status(store, txn),

    StatusCheck { txn } => status_check(store, txn),

    DefineAsset { builder,
                  issuer_nick,
                  asset_nick, } => define_asset(store, builder, issuer_nick, asset_nick),

    IssueAsset { builder,
                 asset_nick,
                 issue_seq_num,
                 amount, } => issue_asset(store, builder, asset_nick, issue_seq_num, amount),

    TransferAssets { builder } => transfer_assets(store, builder),

    BuildTransaction { builder,
                       txn_nick,
                       exact, } => build_transaction(store, builder, txn_nick, exact),

    Submit { nick } => submit(store, nick),
  };
  store.update_config(|conf| {
         // println!("Opened {} times before", conf.open_count);
         conf.open_count += 1;
         Ok(())
       })?;
  ret
}

fn main() {
  fn inner_main() -> Result<(), CliError> {
    let action = Actions::from_args();

    if let Actions::GenCompletions { outdir,
                                     bash,
                                     zsh,
                                     fish,
                                     powershell,
                                     elvish, } = action
    {
      fs::create_dir_all(&outdir).with_context(|| UserFile { file: outdir.clone() })?;

      let bin_name = std::env::args().next().unwrap();

      let mut shells = vec![];
      if bash {
        shells.push(clap::Shell::Bash);
      }
      if zsh {
        shells.push(clap::Shell::Zsh);
      }
      if fish {
        shells.push(clap::Shell::Fish);
      }
      if powershell {
        shells.push(clap::Shell::PowerShell);
      }
      if elvish {
        shells.push(clap::Shell::Elvish);
      }

      for s in shells {
        Actions::clap().gen_completions(&bin_name, s, &outdir);
      }
      return Ok(());
    }

    // use Actions::*;

    let mut home = PathBuf::new();
    match env::var("FINDORA_HOME") {
      Ok(fin_home) => {
        home.push(fin_home);
      }
      Err(_) => {
        home.push(dirs::home_dir().context(HomeDir)?);
        home.push(".findora");
      }
    }
    fs::create_dir_all(&home).with_context(|| UserFile { file: home.clone() })?;
    home.push("cli2_data.sqlite");
    let first_time = !std::path::Path::exists(&home);
    let mut db = KVStore::open(home.clone())?;
    if first_time {
      println!("No config found at {:?} -- triggering first-time setup",
               &home);
      db.update_config(|conf| {
          *conf = prompt_for_config(None).unwrap();
          Ok(())
        })?;

      if let Actions::Setup { .. } = action {
        return Ok(());
      }
    }

    run_action(action, &mut db)?;
    Ok(())
  }

  // Provide a bit of a prettier error message in the event a panic occurs, make the
  // user aware that this is a bug, direct them to the bug tracker, and display a
  // backtrace.
  std::panic::set_hook(Box::new(|panic_info| {
                         // TODO(Nathan M): Add a link to the bug tracker with prefilled information

                         println!("An unknown error occurred, this is a bug, please help us fix it by \
                                   reporting it at: ");
                         println!("https://bugtracker.findora.org/projects/testnet/issues/new");
                         println!("Please copy and paste this entire error message, as well as any preceding \
                                   output into the \ndescription field of the bug report.\n");
                         println!("Here is what context is available:");
                         let payload = panic_info.payload();
                         if let Some(s) = payload.downcast_ref::<&str>() {
                           println!("  {}", s);
                         } else if let Some(s) = payload.downcast_ref::<String>() {
                           println!("  {}", s);
                         }

                         if let Some(location) = panic_info.location() {
                           println!("Error occurred at: {}", location);
                         }

                         println!("\n\n Information for Developers:");
                         println!("{}", Backtrace::generate());
                       }));

  // Custom error handler logic.
  //
  // If the call to `inner_main` encountered an error, display the error it
  // encountered, then make repeated calls to `Error::source` to walk the source
  // list, displaying each error in the chain, in order.
  //
  // Finally, check to see if the error encountered has an associated backtrace, and,
  // if so, display it.
  let ret = inner_main();
  if let Err(x) = ret {
    use snafu::ErrorCompat;
    use std::error::Error;
    let backtrace = ErrorCompat::backtrace(&x);
    println!("Error: {}", x);
    let mut current = &x as &dyn Error;
    while let Some(next) = current.source() {
      println!("   Caused by: {}", next);
      current = next;
    }
    if let Some(backtrace) = backtrace {
      // On a normal error, only print the backtrace if RUST_BACKTRACE is setup
      if std::env::var_os("RUST_BACKTRACE").is_some() {
        println!("Backtrace: \n{}", backtrace);
      } else {
        println!("Rerun with \"env RUST_BACKTRACE=1\" to print a backtrace.");
      }
    }
    std::process::exit(1);
  }
}
