#![deny(warnings)]
use ledger::data_model::AssetTypeCode;
use ledger_standalone::LedgerStandalone;
use std::fs;
use std::io::{self, Write};
use std::process::{Command, Output};
use std::str::from_utf8;

extern crate exitcode;

// TODO (Keyao): Fix tests with #[ignore].
// Those tests pass individually, but occasionally fail when run with other tests.
// They take more time to complete, thus might cause data conflicts.
#[cfg(debug_assertions)]
const COMMAND: &str = "../../target/debug/txn_cli";

#[cfg(not(debug_assertions))]
const COMMAND: &str = "../../target/release/txn_cli";

const DATA_FILE: &str = "data.json";

//
// Helper functions: view records
//
#[cfg(test)]
fn view_loan_all(user_type: &str, user_id: &str) -> io::Result<Output> {
  Command::new(COMMAND).args(&[user_type, "--id", user_id])
                       .arg("view_loan")
                       .output()
}

#[cfg(test)]
fn view_loan_with_loan_id(user_type: &str, user_id: &str, loan_id: &str) -> io::Result<Output> {
  Command::new(COMMAND).args(&[user_type, "--id", user_id])
                       .arg("view_loan")
                       .args(&["--loan", loan_id])
                       .output()
}

#[cfg(test)]
fn view_loan_with_filter(user_type: &str, user_id: &str, filter: &str) -> io::Result<Output> {
  Command::new(COMMAND).args(&[user_type, "--id", user_id])
                       .arg("view_loan")
                       .args(&["--filter", filter])
                       .output()
}

#[cfg(test)]
fn view_credential_all(borrower_id: &str) -> io::Result<Output> {
  Command::new(COMMAND).args(&["borrower", "--id", borrower_id])
                       .arg("view_credential")
                       .output()
}

#[cfg(test)]
fn view_credential_attribute(borrower_id: &str, attribute: &str) -> io::Result<Output> {
  Command::new(COMMAND).args(&["borrower", "--id", borrower_id])
                       .arg("view_credential")
                       .args(&["--attribute", attribute])
                       .output()
}

//
// Helper functions: sign up an account
//
#[cfg(test)]
fn sign_up_borrower(name: &str) -> io::Result<Output> {
  Command::new(COMMAND).args(&["borrower", "sign_up"])
                       .args(&["--name", name])
                       .output()
}

//
// Helper functions: create and store without path
//
#[cfg(test)]
fn create_or_overwrite_credential(id: &str, attribute: &str, value: &str) -> io::Result<Output> {
  Command::new(COMMAND).args(&["borrower", "--id", id])
                       .arg("create_or_overwrite_credential")
                       .args(&["--credential_issuer", "0"])
                       .args(&["--attribute", attribute])
                       .args(&["--value", value])
                       .output()
}

#[cfg(test)]
fn request_loan(lender: &str,
                borrower: &str,
                amount: &str,
                interest_per_mille: &str,
                duration: &str)
                -> io::Result<Output> {
  Command::new(COMMAND).args(&["borrower", "--id", borrower])
                       .arg("request_loan")
                       .args(&["--lender", lender])
                       .args(&["--amount", amount])
                       .args(&["--interest_per_mille", interest_per_mille])
                       .args(&["--duration", duration])
                       .output()
}

//
// Helper functions: create and store with path
//
#[cfg(test)]
fn create_txn_builder_with_path(path: &str) -> io::Result<Output> {
  Command::new(COMMAND).arg("create_txn_builder")
                       .args(&["--name", path])
                       .output()
}

#[cfg(test)]
fn trace_credential(id: &str,
                    memo_file: &str,
                    attribute: &str,
                    expected_value: &str)
                    -> io::Result<Output> {
  Command::new(COMMAND).args(&["asset_issuer", "--id", id])
                       .arg("trace_credential")
                       .args(&["--memo_file", memo_file])
                       .args(&["--attribute", attribute])
                       .args(&["--expected_value", expected_value])
                       .output()
}

//
// Helper functions: define, issue and transfer
//
#[cfg(test)]
fn air_assign(txn_builder_path: &str,
              issuer_id: &str,
              address: &str,
              data: &str)
              -> io::Result<Output> {
  Command::new(COMMAND).args(&["--txn", txn_builder_path])
                       .args(&["asset_issuer", "--id", issuer_id])
                       .arg("air_assign")
                       .args(&["--address", address])
                       .args(&["--data", data])
                       .output()
}

#[cfg(test)]
fn define_asset(txn_builder_path: &str,
                issuer_id: &str,
                token_code: &str,
                memo: &str)
                -> io::Result<Output> {
  Command::new(COMMAND).args(&["--txn", txn_builder_path])
                       .args(&["asset_issuer", "--id", issuer_id])
                       .arg("define_asset")
                       .args(&["--token_code", token_code])
                       .args(&["--memo", memo])
                       .output()
}

#[cfg(test)]
fn issue_asset_with_confidential_amount(txn_builder_path: &str,
                                        id: &str,
                                        token_code: &str,
                                        amount: &str)
                                        -> io::Result<Output> {
  Command::new(COMMAND).args(&["--txn", txn_builder_path])
                       .args(&["asset_issuer", "--id", id])
                       .arg("issue_asset")
                       .args(&["--token_code", token_code])
                       .args(&["--amount", amount])
                       .arg("--confidential_amount")
                       .output()
}

#[cfg(test)]
fn transfer_asset(txn_builder_path: &str,
                  issuer_id: &str,
                  recipient_ids: &str,
                  sids_file: &str,
                  issuance_txn_files: &str,
                  input_amounts: &str,
                  output_amounts: &str)
                  -> io::Result<Output> {
  Command::new(COMMAND).args(&["--txn", txn_builder_path])
                       .args(&["asset_issuer", "--id", issuer_id])
                       .arg("transfer_asset")
                       .args(&["--recipients", recipient_ids])
                       .args(&["--sids_file", sids_file])
                       .args(&["--issuance_txn_files", issuance_txn_files])
                       .args(&["--input_amounts", input_amounts])
                       .args(&["--output_amounts", output_amounts])
                       .output()
}

// Helper functions: submit transaction
// Note:
// Since http://localhost is used instead of https://testnet.findora.org,
// make sure the standalone ledger is running before calling a function that will submit a transaction:
// ```
// let ledger_standalone = LedgerStandalone::new();
// ledger_standalone.poll_until_ready().unwrap();
// ```

#[cfg(test)]
fn submit(txn_builder_path: &str) -> io::Result<Output> {
  Command::new(COMMAND).args(&["--txn", txn_builder_path])
                       .arg("submit")
                       .args(&["--http", "--localhost"])
                       .output()
}

#[cfg(test)]
fn submit_and_store_sids(txn_builder_path: &str, sids_file: &str) -> io::Result<Output> {
  Command::new(COMMAND).args(&["--txn", txn_builder_path])
                       .arg("submit")
                       .args(&["--sids_file", sids_file])
                       .args(&["--http", "--localhost"])
                       .output()
}

// Helper function: load funds
#[cfg(test)]
fn load_funds(issuer_id: &str, borrower_id: &str, amount: &str) -> io::Result<Output> {
  Command::new(COMMAND).args(&["borrower", "--id", borrower_id])
                       .arg("load_funds")
                       .args(&["--issuer", issuer_id])
                       .args(&["--amount", amount])
                       .args(&["--http", "--localhost"])
                       .output()
}

// Helper functions: initiate and pay loan
#[cfg(test)]
fn fulfill_loan(lender_id: &str,
                loan_id: &str,
                issuer_id: &str,
                memo_file: Option<&str>)
                -> io::Result<Output> {
  if let Some(file) = memo_file {
    Command::new(COMMAND).args(&["lender", "--id", lender_id])
                         .arg("fulfill_loan")
                         .args(&["--loan", loan_id])
                         .args(&["--issuer", issuer_id])
                         .args(&["--memo_file", file])
                         .args(&["--http", "--localhost"])
                         .output()
  } else {
    Command::new(COMMAND).args(&["lender", "--id", lender_id])
                         .arg("fulfill_loan")
                         .args(&["--loan", loan_id])
                         .args(&["--issuer", issuer_id])
                         .args(&["--http", "--localhost"])
                         .output()
  }
}

#[cfg(test)]
fn pay_loan(borrower_id: &str, loan_id: &str, amount: &str) -> io::Result<Output> {
  Command::new(COMMAND).args(&["borrower", "--id", borrower_id])
                       .arg("pay_loan")
                       .args(&["--loan", loan_id])
                       .args(&["--amount", amount])
                       .args(&["--http", "--localhost"])
                       .output()
}

#[test]
#[ignore]
fn test_create_or_overwrite_credentials() {
  // Create a borrower
  sign_up_borrower("Borrower B").expect("Failed to create a borrower");

  // Create the credential with minimum credit score record
  let output = create_or_overwrite_credential("1", "min_credit_score", "600").expect("Failed to create a min_credit_score credential");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"Creating the credential record.".to_owned()));

  // Overwrite the minimum credit score record
  let output = create_or_overwrite_credential("1", "min_credit_score", "680").expect("Failed to overwrite the min_credit_score credential");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"Overwriting the credential attribute.".to_owned()));

  // Add the minimum income record to the credential
  let output =
  create_or_overwrite_credential("1", "min_income", "1000").expect("Failed to create a min_income credential");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"Adding the credential attribute.".to_owned()));

  let _ = fs::remove_file(DATA_FILE);
}

//
// Lender or borrower views loans or credentials
//
#[test]
#[ignore]
fn test_view() {
  let ledger_standalone = LedgerStandalone::new();

  // Add a credential
  create_or_overwrite_credential("0", "min_income", "1500").expect("Failed to create a credential");

  // Create loans
  request_loan("0", "0", "100", "100", "3").expect("Failed to request the loan");
  request_loan("0", "0", "200", "150", "6").expect("Failed to request the loan");
  request_loan("1", "0", "300", "200", "9").expect("Failed to request the loan");
  request_loan("1", "0", "500", "300", "15").expect("Failed to request the loan");

  // Fulfill some of the loans
  ledger_standalone.poll_until_ready().unwrap();
  fulfill_loan("0", "0", "0", None).expect("Failed to fulfill the loan");
  ledger_standalone.poll_until_ready().unwrap();
  fulfill_loan("0", "1", "0", None).expect("Failed to fulfill the loan");
  ledger_standalone.poll_until_ready().unwrap();
  fulfill_loan("1", "2", "0", None).expect("Failed to fulfill the loan");

  // View loans
  // 1. View all loans of a lender
  let output = view_loan_all("lender", "1").expect("Failed to view the loan");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"Displaying 2 loan(s):".to_owned()));

  // 2. View all loans of a borrower
  let output = view_loan_all("borrower", "0").expect("Failed to view the loan");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"Displaying 4 loan(s):".to_owned()));

  // 3.   View a loan by its id
  // 3.1  The loan is owned by the user
  let output = view_loan_with_loan_id("lender", "0", "0").expect("Failed to view the loan");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"Displaying loan".to_owned()));

  // 3.2  The loan isn't owned by the user
  let output = view_loan_with_loan_id("lender", "0", "2").expect("Failed to view the loan");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"doesn't own loan".to_owned()));

  // 4. View loans with a filter
  // 4.1 Requested but not fulfilled loan
  let output =
    view_loan_with_filter("borrower", "0", "requested").expect("Failed to view the loan");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"Displaying 1 loan(s):".to_owned()));

  // 4.2. View fulfilled loan
  let output =
    view_loan_with_filter("borrower", "0", "fulfilled").expect("Failed to view the loan");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"Displaying 2 loan(s):".to_owned()));

  // 4.3. View declined loan
  let output = view_loan_with_filter("borrower", "0", "declined").expect("Failed to view the loan");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"Displaying 1 loan(s):".to_owned()));

  // 4.4. View active loan
  let output = view_loan_with_filter("borrower", "0", "active").expect("Failed to view the loan");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"Displaying 2 loan(s):".to_owned()));

  // 4.5. View complete loan
  let output = view_loan_with_filter("borrower", "0", "complete").expect("Failed to view the loan");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"Displaying 0 loan(s):".to_owned()));

  // View credentials
  // 1. View all credentials of a borrower
  let output = view_credential_all("0").expect("Failed to view the loan");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(!from_utf8(&output.stdout).unwrap()
                                    .contains(&"citizenship".to_owned()));

  // 2. View a credential attribute
  let output = view_credential_attribute("0", "min_income").expect("Failed to view the attribute");
  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"Displaying \"min_income\"".to_owned()));

  let _ = fs::remove_file(DATA_FILE);
}

//
// Compose transaction and submit
//
#[test]
#[ignore]
fn test_define_issue_transfer_and_submit_with_args() {
  let ledger_standalone = LedgerStandalone::new();

  // Create users and txn builder files
  sign_up_borrower("Borrower 1").expect("Failed to create a borrower");
  sign_up_borrower("Borrower 2").expect("Failed to create a borrower");
  let creation_txn_builder_file = "tb_define_and_submit";
  let issuance_txn_builder_file = "tb_issue_submit";
  let transfer_txn_builder_file = "tb_transfer_submit";

  // Define asset
  let token_code = AssetTypeCode::gen_random().to_base64();
  let output = define_asset(creation_txn_builder_file,
                            "0",
                            &token_code,
                            "Define an asset").expect("Failed to define asset");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());

  // Submit transaction
  ledger_standalone.poll_until_ready().unwrap();
  let output = submit(creation_txn_builder_file).expect("Failed to submit transaction");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  fs::remove_file(creation_txn_builder_file).unwrap();
  assert!(output.status.success());

  // Issue asset
  let amount_issue = "50";
  let output = issue_asset_with_confidential_amount(issuance_txn_builder_file,
                                                    "0",
                                                    &token_code,
                                                    amount_issue).expect("Failed to issue asset");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());

  // Submit transaction
  let sids_file = "sids_define_issue_transfer_and_submit";
  ledger_standalone.poll_until_ready().unwrap();
  let output =
    submit_and_store_sids(issuance_txn_builder_file, sids_file).expect("Failed to submit transaction");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());

  // Transfer asset
  let output = transfer_asset(transfer_txn_builder_file,
                              "0",
                              "1,2",
                              sids_file,
                              issuance_txn_builder_file,
                              "50",
                              "30, 20").expect("Failed to transfer asset");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  fs::remove_file(issuance_txn_builder_file).unwrap();
  fs::remove_file(sids_file).unwrap();

  assert!(output.status.success());

  // Submit transaction
  ledger_standalone.poll_until_ready().unwrap();
  let output = submit(transfer_txn_builder_file).expect("Failed to submit transaction");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  let _ = fs::remove_file(DATA_FILE);
  fs::remove_file(transfer_txn_builder_file).unwrap();

  assert!(output.status.success());
}

//
// Compose transaction and submit
//
#[ignore]
#[test]
fn test_air_assign() {
  // Create txn builder and key pair
  let txn_builder_file = "tb_air_assign";
  create_txn_builder_with_path(txn_builder_file).expect("Failed to create transaction builder");

  // Air assigning
  air_assign(txn_builder_file, "0", "666", "Hell").expect("Failed to assign to AIR");

  // Submit transaction
  let ledger_standalone = LedgerStandalone::new();
  ledger_standalone.poll_until_ready().unwrap();
  let output = submit(txn_builder_file).expect("Failed to submit transaction");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  let _ = fs::remove_file(DATA_FILE);
  fs::remove_file(txn_builder_file).unwrap();

  assert!(output.status.success());
}

#[test]
#[ignore]
// Test funds loading, loan request, fulfilling and repayment
fn test_request_fulfill_and_pay_loan_with_args() {
  let ledger_standalone = LedgerStandalone::new();

  // Load funds
  ledger_standalone.poll_until_ready().unwrap();
  let output = load_funds("0", "0", "5000").expect("Failed to load funds");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());

  // Request the first loan
  let output = request_loan("0", "0", "1500", "100", "8").expect("Failed to request a loan");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());

  // Request the second loan
  let output = request_loan("1", "0", "1000", "80", "10").expect("Failed to request a loan");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());

  // Fulfill the first loan
  // 1. First time:
  //    Add the credential proof, then successfully initiate the loan
  //    Trace the credential associated with the first loan
  let memo_file = "memo_fulfill_loan_args";
  ledger_standalone.poll_until_ready().unwrap();
  let output_fulfill =
    fulfill_loan("0", "0", "0", Some(memo_file)).expect("Failed to initiate the loan");
  ledger_standalone.poll_until_ready().unwrap();
  let output_trace_fail =
    trace_credential("0", memo_file, "min_income", "1000").expect("Failed to trace the credential");
  ledger_standalone.poll_until_ready().unwrap();
  let output_trace_pass = trace_credential("0", memo_file, "min_credit_score", "650").expect("Failed to trace the credential");

  fs::remove_file(memo_file).unwrap();

  io::stdout().write_all(&output_fulfill.stdout).unwrap();
  io::stdout().write_all(&output_fulfill.stderr).unwrap();

  assert!(output_fulfill.status.success());

  io::stdout().write_all(&output_trace_fail.stdout).unwrap();
  io::stdout().write_all(&output_trace_fail.stderr).unwrap();

  assert!(!output_trace_fail.status.success());

  io::stdout().write_all(&output_trace_pass.stdout).unwrap();
  io::stdout().write_all(&output_trace_pass.stderr).unwrap();

  assert!(output_trace_pass.status.success());

  // 2. Second time:
  //    Fail because the loan has been fulfilled
  ledger_standalone.poll_until_ready().unwrap();
  let output = fulfill_loan("0", "0", "0", None).expect("Failed to initiate the loan");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert_eq!(output.status.code(), Some(exitcode::USAGE));
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"has already been fulfilled.".to_owned()));

  // Fulfill the second loan
  // 1. First time:
  //    Get the credential proof, then fail to initiate the loan because the requirement isn't met
  ledger_standalone.poll_until_ready().unwrap();
  let output = fulfill_loan("1", "1", "0", None).expect("Failed to initiate the loan");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert_eq!(output.status.code(), Some(exitcode::USAGE));
  let stdout = from_utf8(&output.stdout).unwrap();
  assert!(stdout.contains(&"should be at least:".to_owned()));

  // 2. Second time:
  //    Fail because the loan has been declined
  ledger_standalone.poll_until_ready().unwrap();
  let output = fulfill_loan("1", "1", "0", None).expect("Failed to initiate the loan");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert_eq!(output.status.code(), Some(exitcode::USAGE));
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"has already been declined.".to_owned()));

  // Pay loan
  // 1. First time:
  //    Burn part of the loan balance
  ledger_standalone.poll_until_ready().unwrap();
  let output = pay_loan("0", "0", "300").expect("Failed to pay loan");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());

  // 2. Second time
  //    Pay off the loan
  ledger_standalone.poll_until_ready().unwrap();
  let output = pay_loan("0", "0", "2000").expect("Failed to pay loan");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert!(output.status.success());

  // 3. Third time:
  //    Fail because the loan has been paid off
  ledger_standalone.poll_until_ready().unwrap();
  let output = pay_loan("0", "0", "3000").expect("Failed to pay loan");

  io::stdout().write_all(&output.stdout).unwrap();
  io::stdout().write_all(&output.stderr).unwrap();

  assert_eq!(output.status.code(), Some(exitcode::USAGE));
  assert!(from_utf8(&output.stdout).unwrap()
                                   .contains(&"has been paid off.".to_owned()));

  let _ = fs::remove_file(DATA_FILE);
}
