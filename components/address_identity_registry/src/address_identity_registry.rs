//#![deny(warnings)]
#![feature(slice_patterns)]
// Copyright 2019 © Findora. All rights reserved.
/// Command line executable to exercise functions related to credentials
#[macro_use]
extern crate lazy_static;

use clap;
use clap::{App, Arg, ArgMatches};
use colored::*;
use cryptohash::sha256;
use hex;
use rand_chacha::ChaChaRng;
use rand_core::SeedableRng;
use rmp_serde;
use rustyline::error::ReadlineError;
use rustyline::Editor;
// use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};
use sha256::DIGESTBYTES;
use sparse_merkle_tree::SmtMap256;
use std::collections::HashMap;
use std::path::Path;
use zei::api::anon_creds::{
  ac_keygen_issuer, ac_keygen_user, ac_reveal, ac_reveal_with_rand, ac_sample_random_factors,
  ac_sign, ac_verify, ACIssuerPublicKey, ACIssuerSecretKey, ACRevealSig, ACUserPublicKey,
  ACUserSecretKey,
};

// Default file path of the anonymous credential registry
const DEFAULT_REGISTRY_PATH: &str = "acreg.json";

const HELP_STRING: &str = r#"
  Commands:
    help:
      Prints this message
    addissuer <issuer_name>:
      Creates an issuer which can be referred to by the name "issuer_name"
    adduser <issuer_name> <user_name>:
      Creates an user named "user_name" bound to "issuer_name
    issue <issuer_name> <user_name> [<attr_name>]*:
      Creates a signature
    reveal <user_name> <issuer_name>:
      Creates a proof for a signature
    verify <user_name> <issuer_name>:
      Verifies the proof

Example of use
  >>> addissuer bank0
  >>> adduser bank0 user0
  >>> issue user0 bank0
  >>> reveal user0 bank0
  >>> verify user0 bank0
  etc...
"#;
lazy_static! {
  static ref COMMANDS: HashMap<&'static str, &'static str> = {
    let mut m = HashMap::new();
    m.insert("help", HELP_STRING);
    m.insert("addissuer", "");
    m.insert("adduser", "");
    m.insert("sign", "");
    m.insert("reveal", "");
    m.insert("verify", "");
    m
  };
}

const ATTRIBUTES: [&[u8]; 4] = [b"attr0", b"attr1", b"attr2", b"attr3"];

#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct Hash256([u8; DIGESTBYTES]);

const ZERO_DIGEST: Hash256 = Hash256([0; DIGESTBYTES]);

impl std::fmt::Display for Hash256 {
  fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    write!(f, "{}", hex::encode(&self.0))
  }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Issuer {
  public_key: ACIssuerPublicKey,
  secret_key: ACIssuerSecretKey,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct User {
  public_key: ACUserPublicKey,
  secret_key: ACUserSecretKey,
}

// TypeName is used for hash salting
trait TypeName {
  fn type_string(&self) -> &'static str;
}

impl TypeName for ACUserPublicKey {
  fn type_string(&self) -> &'static str {
    "ACUserPublicKey"
  }
}

impl TypeName for ACIssuerPublicKey {
  fn type_string(&self) -> &'static str {
    "ACIssuerPublicKey"
  }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Credential {
  cred: ACRevealSig,
  attrs: Vec<String>,
}

#[derive(Debug)]
struct GlobalState {
  prng: ChaChaRng,
  registry: Vec<String>, // Not used anymore, used to represent file storage
  smt_map: SmtMap256<String>,
  users: HashMap<String, User>,
  issuers: HashMap<String, Issuer>,
  user_issuer: HashMap<String, Issuer>, // Each user has a single issuer
  user_cred: HashMap<String, Credential>, // Each user has at most a single credential issued to it
}

impl GlobalState {
  fn new() -> Self {
    GlobalState { prng: ChaChaRng::from_seed([0u8; 32]),
                  registry: Vec::<String>::new(),
                  smt_map: SmtMap256::<String>::new(),
                  users: HashMap::new(),
                  issuers: HashMap::new(),
                  user_issuer: HashMap::new(),
                  user_cred: HashMap::new() }
  }
}

fn hash_256(value: impl AsRef<[u8]>) -> Hash256 {
  Hash256(sha256::hash(value.as_ref()).0)
}

// Return the SHA256 hash of T as a hexadecimal string.
fn sha256<T>(key: &T) -> Hash256
  where T: Serialize + TypeName
{
  println!("sha256: hashing type: {}",
           key.type_string().to_string().cyan());
  // Salt the hash to avoid leaking information about other uses of
  // sha256 on the user's public key.
  let mut bytes = key.type_string().to_string().into_bytes();
  // TODO: Verify that when we do serialize into a non-empty vector that we
  //       do NOT overwrite the existing vec data but rather push data onto it.
  key.serialize(&mut rmp_serde::Serializer::new(&mut bytes))
     .unwrap();
  hash_256(bytes)
}

// Test anonymous credentials on fixed inputs. Similar to
// Zei's credentials_tests.

fn test(global_state: &mut GlobalState) -> Result<(), String> {
  // Attributes to be revealed. For example, they might be:
  //    account balance, zip code, credit score, and timestamp
  // In this case, account balance (first) will not be revealed.
  let bitmap = [false, true, true, true];
  let attrs = [92_574_500u64.to_le_bytes(),
               95_050u64.to_le_bytes(),
               720u64.to_le_bytes(),
               20_190_820u64.to_le_bytes()];
  let att_count = bitmap.len();
  let (issuer_pk, issuer_sk) = ac_keygen_issuer::<_>(&mut global_state.prng, att_count);

  println!("Issuer public key: {:?}", issuer_pk);
  println!("Issuer secret key: {:?}", issuer_sk);

  let (user_pk, user_sk) = ac_keygen_user::<_>(&mut global_state.prng, &issuer_pk);
  println!("User public key: {:#?}", user_pk);
  println!("Address of user public key: {:?}", sha256(&user_pk));

  // The user secret key holds [u64; 6], but with more structure.
  println!("User secret key: {:?}", user_sk);

  // Issuer vouches for the user's attributes given above.
  let sig = ac_sign(&mut global_state.prng, &issuer_sk, &user_pk, &attrs[..]);
  println!("Credential signature: {:?}", sig);

  // The user presents this to the second party in a transaction as proof
  // attributes have been committed without revealing the values.
  let reveal_sig = ac_reveal(&mut global_state.prng,
                             &user_sk,
                             &issuer_pk,
                             &sig,
                             &attrs,
                             &bitmap).unwrap();

  // Decision point. Does the second party agree to do business?
  // Sometimes this is presumed such as a syndicated investment
  // round, where you'll take money from everyone qualified. Other
  // times, there might be an off-chain negotiation to decide
  // whether to provisionally accept the deal.

  let mut revealed_attrs = vec![];
  for (attr, b) in attrs.iter().zip(&bitmap) {
    if *b {
      revealed_attrs.push(attr.clone());
    }
  }

  // Proves the attributes are what the user committed to. Anyone
  // with the revealed attributes and the reveal signature can do
  // this. But presumably, the reveal signature alone is insufficient to
  // derive the attributes. Presumably if the range of legal values were small,
  // exhaustive search would not be too exhausting. (?)
  if let Err(e) = ac_verify(&issuer_pk,
                            revealed_attrs.as_slice(),
                            &bitmap,
                            &reveal_sig.sig,
                            &reveal_sig.pok)
  {
    Err(format!("{}", e))
  } else {
    Ok(())
  }
}

// Generate a new issuer and append it to the registry.
fn add_issuer(mut global_state: &mut GlobalState, issuer_name: &str) -> Result<(), String> {
  // Generate a new issuer for anonymous credentials.
  fn new_issuer(global_state: &mut GlobalState) -> Issuer {
    let att_count = 10;
    let (issuer_pk, issuer_sk) = ac_keygen_issuer::<_>(&mut global_state.prng, att_count);
    Issuer { public_key: issuer_pk,
             secret_key: issuer_sk }
  }
  if let Some(_) = global_state.issuers.get(issuer_name) {
    Err(format!("issuer named {} already exists", issuer_name))
  } else {
    let issuer = new_issuer(&mut global_state);
    global_state.issuers.insert(issuer_name.to_string(), issuer);
    println!("New issuer {}", issuer_name.yellow());
    Ok(())
  }
}

fn add_user(global_state: &mut GlobalState,
            issuer_name: &str,
            user_name: &str)
            -> Result<(), String> {
  if let Some(_) = global_state.users.get(user_name) {
    Err(format!("user named {} already exists", user_name))
  } else {
    // println!("Looking up issuer: {}", issuer_name);
    if let Some(issuer) = global_state.issuers.get(issuer_name) {
      let (user_pk, user_sk) = ac_keygen_user::<_>(&mut global_state.prng, &issuer.public_key);
      let user = User { public_key: user_pk,
                        secret_key: user_sk };
      println!("New user {} with issuer {}", user_name, issuer_name);
      global_state.users.insert(user_name.to_string(), user);
      global_state.user_issuer
                  .insert(user_name.to_string(), issuer.clone());
      Ok(())
    } else {
      Err(format!("lookup of issuer {} failed", issuer_name))
    }
  }
}

fn issue_credential(global_state: &mut GlobalState,
                    user: &str,
                    attrs: &Vec<String>)
                    -> Result<(), String> {
  match (global_state.users.get(user), global_state.user_issuer.get(user)) {
    (Some(user_keys), Some(issuer_keys)) => {
      let sig = ac_sign(&mut global_state.prng,
                        &issuer_keys.secret_key,
                        &user_keys.public_key,
                        &attrs);

      // User generates cred by calling ac_reveal with no attributes
      let empty_attrs: Vec<String> = Vec::new();
      let empty_bitmap: Vec<bool> = Vec::new();
      if let Ok(proof) = ac_reveal(&mut global_state.prng,
                                   &user_keys.secret_key,
                                   &issuer_keys.public_key,
                                   &sig,
                                   &empty_attrs,
                                   &empty_bitmap)
      {
        // Insert an entry AIR[user_pk] = cred, where
        let sig_string = serde_json::to_string(&proof.sig).unwrap();
        let user_addr = sha256(&user_keys.public_key);
        global_state.smt_map.set(&user_addr.0, Some(sig_string));
        let credential = Credential { cred: proof,
                                      attrs: attrs.to_vec() };
        global_state.user_cred.insert(user.to_string(), credential);
        Ok(())
      } else {
        Err("Credential generation fails during credential issuing process".to_string())
      }
    }
    (None, None) => Err("Unable to find either issuer or user".to_string()),
    (None, _) => Err("Unable to find user".to_string()),
    (_, None) => Err("Unable to find issuer".to_string()),
  }
}

fn reveal(global_state: &mut GlobalState, user: &str, bitmap: &Vec<bool>) -> Result<(), String> {
  match (global_state.users.get(user),
         global_state.user_issuer.get(user),
         global_state.user_cred.get(user))
  {
    (Some(user_keys), Some(issuer_keys), Some(cred)) => {
      if let Ok(proof) = ac_reveal_with_rand(&mut global_state.prng,
                                             &user_keys.secret_key,
                                             &issuer_keys.public_key,
                                             &cred.cred.sig,
                                             &cred.attrs,
                                             &bitmap,
                                             cred.cred.rnd.clone())
      {
        // global_state.smt.get()
        // TODO Using the hash of the user's public key as the proof
        // address precludes multiple proofs

        // global_state.user_reveal.insert(user.to_string(), proof);
        Ok(())
      } else {
        Err("ac_reveal failed".to_string())
      }
    }
    (None, _, _) => Err("Unable to find user".to_string()),
    (_, None, _) => Err("Unable to find issuer".to_string()),
    (_, _, None) => Err("Unable to find signature".to_string()),
  }
}

fn verify(global_state: &mut GlobalState, user: &str, bitmap: &Vec<bool>) -> Result<(), String> {
  println!("Command verify user {} with bitmap {:?}", user, bitmap);
  match (global_state.user_issuer.get(user), global_state.user_cred.get(user)) {
    (Some(issuer_keys), Some(cred)) => {
      if let Err(e) = ac_verify(&issuer_keys.public_key,
                                &cred.attrs,
                                &bitmap,
                                &cred.cred.sig,
                                &cred.cred.pok)
      {
        Err(format!("{}", e))
      } else {
        // Check merkle proof here?
        Ok(())
      }
    }
    (None, _) => Err("Unable to find issuer".to_string()),
    (_, None) => Err("Unable to find credentials".to_string()),
  }
}

fn issuer_exists(global_state: &GlobalState, issuer: &str) -> Result<(), String> {
  if let Some(_) = global_state.issuers.get(issuer) {
    Ok(())
  } else {
    Err(format!("{} is not a valid issuer name", issuer))
  }
}

fn user_exists(global_state: &GlobalState, user: &str) -> Result<(), String> {
  if let Some(_) = global_state.users.get(user) {
    Ok(())
  } else {
    Err(format!("{} is not a valid user name", user))
  }
}

fn parse_args() -> ArgMatches<'static> {
  App::new("Test REPL").version("0.1.0")
                       .author("Brian Rogoff <brian@findora.org>")
                       .about("REPL with argument parsing")
                       .arg(Arg::with_name("registry").short("r")
                                                      .long("registry")
                                                      .takes_value(true)
                                                      .help("the registry dude"))
                       .arg(Arg::with_name("file").short("f")
                                                  .takes_value(true)
                                                  .help("Name of the file"))
                       .get_matches()
}

fn str_to_bool(s: &str) -> bool {
  let c = s.chars().next().unwrap();
  c == 'T' || c == 't'
}

fn exec_line(mut global_state: &mut GlobalState, line: &str) -> Result<(), String> {
  match line.trim().split(' ').collect::<Vec<&str>>().as_slice() {
    ["help"] => {
      println!("{}", HELP_STRING.green());
      Ok(())
    }
    ["test"] => test(&mut global_state),
    ["addissuer", issuer] => add_issuer(&mut global_state, &issuer),
    ["adduser", issuer, user] => {
      issuer_exists(&global_state, &issuer)?;
      add_user(&mut global_state, &issuer, &user)
    }
    ["getcreds", user, attrs @ ..] => {
      user_exists(&global_state, &user)?;
      let attrs_vec: Vec<String> = attrs.to_vec()
                                        .into_iter()
                                        .map(|s: &str| -> String { s.to_string() })
                                        .collect();
      issue_credential(&mut global_state, &user, &attrs_vec)
    }
    ["reveal", user, bits @ ..] => {
      user_exists(&global_state, &user)?;
      let bitmap: Vec<bool> = bits.to_vec().into_iter().map(str_to_bool).collect();
      reveal(&mut global_state, &user, &bitmap)
    }
    ["verify", user, bits @ ..] => {
      user_exists(&global_state, &user)?;
      let bitmap: Vec<bool> = bits.to_vec().into_iter().map(str_to_bool).collect();
      verify(&mut global_state, &user, &bitmap)
    }
    _ => Err(format!("Invalid line: {}", line.red())),
  }
}

fn main() -> Result<(), rustyline::error::ReadlineError> {
  let args = parse_args();

  let _registry_path = Path::new(args.value_of("registry").unwrap_or(DEFAULT_REGISTRY_PATH));

  let mut global_state = GlobalState::new();

  // println!("The registry path is {}", _registry_path);

  // `()` can be used when no completer is required
  let mut rl = Editor::<()>::new();
  if rl.load_history("history.txt").is_err() {
    println!("No previous history.");
  }

  loop {
    let readline = rl.readline(">>> ");
    match readline {
      Ok(line) => {
        rl.add_history_entry(line.as_str());
        if let Err(e) = exec_line(&mut global_state, &line) {
          println!("Error: {}", e.red());
        } else {
          println!("Success: {}", line.blue());
        }
      }
      Err(ReadlineError::Interrupted) => {
        println!("CTRL-C");
        break;
      }
      Err(ReadlineError::Eof) => {
        println!("CTRL-D");
        break;
      }
      Err(err) => {
        println!("Error: {}", err);
        break;
      }
    }
  }
  rl.save_history("history.txt")
}
