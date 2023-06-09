use ethabi::Contract;
use ethereum_types::H160;
use std::fs;
use std::path::Path;
use std::process::Command;

pub struct ContractConstructor {
    pub abi: Contract,
    pub code: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeployedContract {
    pub abi: Contract,
    pub address: H160,
}

impl ContractConstructor {
    // Note: `contract_file` must be relative to `sources_root`
    pub fn compile_from_source<P1, P2, P3>(
        sources_root: P1,
        artifacts_base_path: P2,
        contract_file: P3,
        contract_name: &str,
    ) -> Self
    where
        P1: AsRef<Path>,
        P2: AsRef<Path>,
        P3: AsRef<Path>,
    {
        let bin_file = format!("{contract_name}.bin");
        let abi_file = format!("{contract_name}.abi");
        let hex_path = artifacts_base_path.as_ref().join(bin_file);
        let hex_rep = match std::fs::read_to_string(&hex_path) {
            Ok(hex) => hex,
            Err(_) => {
                // An error occurred opening the file, maybe the contract hasn't been compiled?
                compile(sources_root, contract_file, &artifacts_base_path);
                // If another error occurs, then we can't handle it so we just unwrap.
                std::fs::read_to_string(hex_path).unwrap()
            }
        };
        let code = hex::decode(hex_rep).unwrap();
        let abi_path = artifacts_base_path.as_ref().join(abi_file);
        let reader = std::fs::File::open(abi_path).unwrap();
        let abi = ethabi::Contract::load(reader).unwrap();

        Self { abi, code }
    }
}

/// Compiles a solidity contract. `source_path` gives the directory containing all solidity
/// source files to consider (including imports). `contract_file` must be
/// given relative to `source_path`. `output_path` gives the directory where the compiled
/// artifacts are written. Requires Docker to be installed.
fn compile<P1, P2, P3>(source_path: P1, contract_file: P2, output_path: P3)
where
    P1: AsRef<Path>,
    P2: AsRef<Path>,
    P3: AsRef<Path>,
{
    let source_path = fs::canonicalize(source_path).unwrap();
    fs::create_dir_all(&output_path).unwrap();
    let output_path = fs::canonicalize(output_path).unwrap();
    let source_mount_arg = format!("{}:/contracts", source_path.to_str().unwrap());
    let output_mount_arg = format!("{}:/output", output_path.to_str().unwrap());
    let contract_arg =
        format!("/contracts/{}", contract_file.as_ref().to_str().unwrap());
    let output = Command::new("docker")
        .args([
            "run",
            "-v",
            &source_mount_arg,
            "-v",
            &output_mount_arg,
            "ethereum/solc:stable",
            "--allow-paths",
            "/contracts/",
            "-o",
            "/output",
            "--abi",
            "--bin",
            "--overwrite",
            &contract_arg,
        ])
        .output()
        .unwrap();
    println!("{}", String::from_utf8(output.stdout).unwrap());
    if !output.status.success() {
        panic!("{}", String::from_utf8(output.stderr).unwrap());
    }
}
