use crate::data_model::errors::PlatformError;
use crate::data_model::*;
use crate::policies::{compute_debt_swap_effect, DebtSwapEffect};
use crate::policy_script::{run_txn_check, TxnCheckInputs, TxnPolicyData};
use crate::{inp_fail, inv_fail, zei_fail};
use credentials::credential_verify_commitment;
use rand_chacha::ChaChaRng;
use rand_core::SeedableRng;
use serde::Serialize;
use sparse_merkle_tree::Key;
use std::collections::{HashMap, HashSet};
use std::iter::once;
use utils::{HasInvariants, HashOf, SignatureOf};
use zei::serialization::ZeiFromToBytes;
use zei::xfr::lib::{verify_xfr_body, XfrNotePolicies};
use zei::xfr::sig::XfrPublicKey;
// use zei::xfr::structs::{TracingPolicies, BlindAssetRecord, XfrAmount, XfrAssetType};
use ruc::*;
use zei::xfr::structs::{TracingPolicies, XfrAmount, XfrAssetType};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TxnEffect {
    // The Transaction object this represents
    pub txn: Transaction,
    // Internally-spent TXOs are None, UTXOs are Some(...)
    pub txos: Vec<Option<TxOutput>>,
    // Which TXOs this consumes
    pub input_txos: HashMap<TxoSID, TxOutput>,
    // List of internally-spent TXOs. This does not include input txos;
    pub internally_spent_txos: Vec<TxOutput>,
    // Which new asset types this defines
    pub new_asset_codes: HashMap<AssetTypeCode, AssetType>,
    // Which new TXO issuance sequence numbers are used, in sorted order
    // The vec should be nonempty unless this asset code is being created in
    // this transaction.
    pub new_issuance_nums: HashMap<AssetTypeCode, Vec<u64>>,
    // Which public key is being used to issue each asset type
    pub issuance_keys: HashMap<AssetTypeCode, IssuerPublicKey>,
    // New issuance amounts
    pub issuance_amounts: HashMap<AssetTypeCode, u64>,
    // Asset types that have issuances with confidential outputs. Issuances cannot be confidential
    // if there is an issuance cap
    pub confidential_issuance_types: HashSet<AssetTypeCode>,
    // Mapping of (op index, xfr input idx) tuples to set of valid signature keys
    // i.e. (2, 1) -> { AlicePk, BobPk } means that Alice and Bob both have valid signatures on the 2nd input of the 1st
    // operation
    pub cosig_keys: HashMap<(usize, usize), HashSet<Vec<u8>>>,
    // Debt swap information that must be externally validated
    pub debt_effects: HashMap<AssetTypeCode, DebtSwapEffect>,
    // Non-confidential asset types involved in confidential transfers
    pub confidential_transfer_inputs: HashSet<AssetTypeCode>,
    // Tracing policies that input/outputs types were validated under
    pub tracing_policies: HashMap<AssetTypeCode, TracingPolicies>,

    pub asset_types_involved: HashSet<AssetTypeCode>,
    pub custom_policy_asset_types: HashMap<AssetTypeCode, TxnCheckInputs>,
    // Updates to the AIR
    pub air_updates: HashMap<String, String>,
    // User-provided Key-Value store updates
    pub kv_updates: HashMap<Key, Vec<(KVEntrySignature, u64, Option<KVEntry>)>>,
    // Memo updates
    pub memo_updates: Vec<(AssetTypeCode, XfrPublicKey, Memo)>,
}

// Internally validates the transaction as well.
// If the transaction is invalid, it is dropped, so if you need to inspect
// the transaction in order to diagnose the error, clone it first!
#[allow(clippy::cognitive_complexity)]
impl TxnEffect {
    pub fn compute_effect(txn: Transaction) -> Result<TxnEffect> {
        let mut txo_count: usize = 0;
        let mut op_idx: usize = 0;
        let mut txos: Vec<Option<TxOutput>> = Vec::new();
        let mut internally_spent_txos = Vec::new();
        let mut input_txos: HashMap<TxoSID, TxOutput> = HashMap::new();
        let mut memo_updates = Vec::new();
        let mut new_asset_codes: HashMap<AssetTypeCode, AssetType> = HashMap::new();
        let mut cosig_keys = HashMap::new();
        let mut new_issuance_nums: HashMap<AssetTypeCode, Vec<u64>> = HashMap::new();
        let mut issuance_keys: HashMap<AssetTypeCode, IssuerPublicKey> = HashMap::new();
        let mut issuance_amounts: HashMap<AssetTypeCode, u64> = HashMap::new();
        let mut debt_effects: HashMap<AssetTypeCode, DebtSwapEffect> = HashMap::new();
        let mut tracing_policies: HashMap<AssetTypeCode, TracingPolicies> =
            HashMap::new();
        let mut asset_types_involved: HashSet<AssetTypeCode> = HashSet::new();
        let mut confidential_issuance_types = HashSet::new();
        let mut kv_updates = HashMap::<
            Key,
            Vec<(
                SignatureOf<(Key, u64, Option<KVEntry>)>,
                u64,
                Option<KVEntry>,
            )>,
        >::new();
        let mut confidential_transfer_inputs = HashSet::new();

        let custom_policy_asset_types = txn
            .body
            .policy_options
            .clone()
            .unwrap_or_else(TxnPolicyData::default)
            .0
            .drain(..)
            .collect::<HashMap<_, _>>();

        let mut air_updates: HashMap<String, String> = HashMap::new();
        let mut params = zei::setup::PublicParams::default(); // TODO pass these in
        let mut prng = ChaChaRng::from_entropy();

        // Sequentially go through the operations, validating intrinsic or
        // local-to-the-transaction properties, then recording effects and
        // external properties.
        //
        // Incrementally recording operations in this way is necessary since
        // validity can depend upon earlier operations within a single
        // transaction (eg, a single transaction containing two Transfers which
        // consume the same TXO is invalid).
        //
        // This process should be a complete internal check of a transaction.
        // In particular, functions consuming a TxnEffect should be able to
        // assume that all internal consistency checks are valid, and that the
        // validity of the whole transaction now only depends on the
        // relationship between the outside world and the TxnEffect's fields
        // (eg, any input TXO SIDs of a Transfer should be recorded in
        // `input_txos` and that Transfer should be valid if all those TXO SIDs
        // exist unspent in the ledger and correspond to the correct
        // TxOutput).
        for op in txn.body.operations.iter() {
            debug_assert!(txo_count == txos.len());

            match op {
                Operation::KVStoreUpdate(update) => {
                    // If there is a prior update, this change must be signed by
                    // the key associated with that update, and must have the
                    // exactly-subsequent generation value.
                    if let Some((_, gen, Some(ent))) =
                        kv_updates.get(&update.body.0).and_then(|x| x.last())
                    {
                        if gen + 1 != update.body.1 {
                            return Err(eg!(PlatformError::InputsError(None)));
                        }
                        update.check_signature(&ent.0).c(d!())?;
                    }
                    // When inserting a value, ensure that the owning key has
                    // signed this update
                    if let Some(ent) = &update.body.2 {
                        update.check_signature(&ent.0).c(d!())?;
                    }
                    kv_updates
                        .entry(update.body.0)
                        .or_insert_with(std::vec::Vec::new)
                        .push((
                            update.signature.clone(),
                            update.body.1,
                            update.body.2.clone(),
                        ));
                }

                // An asset creation is valid iff:
                //     1) The signature is valid.
                //         - Fully checked here
                //     2) The token id is available.
                //         - Partially checked here
                //     3) The policy, if provided, passes its init check
                //         - Fully checked here
                Operation::DefineAsset(def) => {
                    // (1)
                    // TODO(joe?): like the note in data_model, should the public key
                    // used here match `def.body.asset.issuer`?
                    def.signature
                        .verify(&def.pubkey.key, &def.body)
                        .c(d!(zei_fail!()))?;

                    let code = def.body.asset.code;
                    let token = AssetType {
                        properties: *def.body.asset.clone(),
                        ..Default::default()
                    };

                    // (2), only within this transaction
                    if new_asset_codes.contains_key(&code)
                        || new_issuance_nums.contains_key(&code)
                    {
                        return Err(eg!(inp_fail!()));
                    }

                    // (3)
                    if let Some((ref pol, ref globals)) = def.body.asset.policy {
                        let globals = globals.clone();
                        run_txn_check(
                            &pol.init_check,
                            globals.id_vars,
                            globals.rt_vars,
                            globals.amt_vars,
                            globals.frac_vars,
                            &Transaction::from_seq_id(
                                txn.body.no_replay_token.get_seq_id(),
                            ),
                        )
                        .c(d!())?;
                    }

                    issuance_keys.insert(code, token.properties.issuer);
                    new_asset_codes.insert(code, token);
                    new_issuance_nums.insert(code, vec![]);
                }

                // The asset issuance is valid iff:
                //      1) The operation is unique (not a replay).
                //          - Partially checked here
                //      2) The signature is valid.
                //          - Fully checked here
                //      3) The signature belongs to the anchor (the issuer).
                //          - Either checked here or recorded in `new_issuance_keys`
                //      4) The assets in the TxOutputs are owned by the signatory.
                //          - Fully checked here
                //      5) The assets in the TxOutputs have a non-confidential
                //         asset type which agrees with the stated asset type.
                //          - Fully checked here
                Operation::IssueAsset(iss) => {
                    if iss.body.num_outputs != iss.body.records.len() {
                        return Err(eg!(inp_fail!()));
                    }

                    debug_assert!(iss.body.num_outputs == iss.body.records.len());

                    let code = iss.body.code;
                    let seq_num = iss.body.seq_num;

                    asset_types_involved.insert(code);

                    // (1), within this transaction
                    //let v = vec![];
                    let iss_nums =
                        new_issuance_nums.entry(code).or_insert_with(Vec::new);

                    if let Some(last_num) = iss_nums.last() {
                        if seq_num <= *last_num {
                            return Err(eg!(inp_fail!()));
                        }
                    }
                    iss_nums.push(seq_num);

                    // (2)
                    iss.signature
                        .verify(&iss.pubkey.key, &iss.body)
                        .c(d!(zei_fail!()))?;

                    // (3)
                    if let Some(prior_key) = issuance_keys.get(&code) {
                        if iss.pubkey != *prior_key {
                            return Err(eg!(inp_fail!()));
                        }
                    } else {
                        issuance_keys.insert(code, iss.pubkey);
                    }
                    // Increment amounts
                    txos.reserve(iss.body.records.len());
                    for (output, _) in iss.body.records.iter() {
                        // (4)
                        if output.record.public_key != iss.pubkey.key {
                            return Err(eg!(inp_fail!()));
                        }

                        // ONLY SIMPLE TxOutputs!
                        if output
                            != &(TxOutput {
                                id: None,
                                record: output.record.clone(),
                                lien: None,
                            })
                        {
                            return Err(eg!(inp_fail!()));
                        }

                        // (5)
                        if output.record.asset_type
                            != XfrAssetType::NonConfidential(code.val)
                        {
                            return Err(eg!(inp_fail!()));
                        }

                        if let XfrAmount::NonConfidential(amt) = output.record.amount {
                            let issuance_amount =
                                issuance_amounts.entry(code).or_insert(0);
                            *issuance_amount =
                                (*issuance_amount).checked_add(amt).c(d!())?;
                        } else {
                            confidential_issuance_types.insert(code);
                        }

                        txos.push(Some(output.clone()));
                        txo_count += 1;
                    }
                }

                // An asset transfer is valid iff:
                //     1) The signatures on the body (a) all are valid and (b)
                //        there is a signature for each non-custom-policy input key
                //          - Fully checked here
                //     2) The UTXOs (a) exist on the ledger and (b) match the zei transaction.
                //          - Partially checked here -- anything which hasn't
                //            been checked will appear in `input_txos`
                //     3) The zei transaction is valid.
                //          - Checked here and in check_txn_effects
                //     4) Lien assignments match up
                //          - Checked within a transaction here, recorded for
                //            external checks later
                //          - For simplicity, only Standard transfers are allowed
                //            to have lien assignments
                Operation::TransferAsset(trn) => {
                    if trn.body.inputs.len() != trn.body.transfer.inputs.len() {
                        return Err(eg!(inp_fail!()));
                    }
                    if trn.body.outputs.len() != trn.body.transfer.outputs.len() {
                        return Err(eg!(inp_fail!()));
                    }
                    debug_assert!(
                        trn.body.inputs.len() == trn.body.transfer.inputs.len()
                    );
                    debug_assert!(
                        trn.body.outputs.len() == trn.body.transfer.outputs.len()
                    );

                    // Transfer outputs must match outputs zei transaction
                    for (output, record) in trn
                        .body
                        .outputs
                        .iter()
                        .zip(trn.body.transfer.outputs.iter())
                    {
                        if output.record != *record {
                            return Err(eg!(inp_fail!()));
                        }
                    }

                    // Simplify (4)
                    if !trn.body.lien_assignments.is_empty()
                        && trn.body.transfer_type != TransferType::Standard
                    {
                        return Err(eg!(inp_fail!()));
                    }
                    let (lien_inputs, lien_outputs) = {
                        let mut inps = trn
                            .body
                            .transfer
                            .inputs
                            .iter()
                            .map(|_| None)
                            .collect::<Vec<_>>();
                        let mut outs = trn
                            .body
                            .transfer
                            .outputs
                            .iter()
                            .map(|_| None)
                            .collect::<Vec<_>>();
                        for (inp_ix, out_ix, hash) in trn.body.lien_assignments.iter() {
                            let (inp_ix, out_ix) = (*inp_ix, *out_ix);
                            match (inps.get(inp_ix), outs.get(out_ix)) {
                                (Some(None), Some(None)) => {
                                    inps[inp_ix] = Some(hash);
                                    outs[out_ix] = Some(hash);
                                }
                                _ => {
                                    return Err(eg!(inp_fail!()));
                                }
                            }
                        }
                        (inps, outs)
                    };

                    match trn.body.transfer_type {
                        TransferType::DebtSwap => {
                            let (debt_type, debt_swap_effect) =
                                compute_debt_swap_effect(&trn.body.transfer).c(d!())?;

                            if debt_effects.contains_key(&debt_type) {
                                return Err(eg!(inp_fail!()));
                            }
                            debt_effects.insert(debt_type, debt_swap_effect);
                        }
                        TransferType::Standard => {
                            let mut input_keys = HashSet::new();
                            // (1a) all body signatures are valid
                            for sig in &trn.body_signatures {
                                if !trn.body.verify_body_signature(sig) {
                                    return Err(eg!(inp_fail!()));
                                }
                                if let Some(input_idx) = sig.input_idx {
                                    let sig_keys = cosig_keys
                                        .entry((op_idx, input_idx))
                                        .or_insert_with(HashSet::new);
                                    (*sig_keys).insert(sig.address.key.zei_to_bytes());
                                } else {
                                    input_keys.insert(sig.address.key.zei_to_bytes());
                                }
                            }

                            // (1b) all input record owners (for non-custom-policy
                            //      assets) have signed
                            for (input_idx, record) in
                                trn.body.transfer.inputs.iter().enumerate()
                            {
                                // skip signature checking for custom-policy assets
                                if let Some(inp_code) =
                                    record.asset_type.get_asset_type()
                                {
                                    if custom_policy_asset_types
                                        .get(&AssetTypeCode { val: inp_code })
                                        .is_some()
                                    {
                                        continue;
                                    }
                                }
                                if !input_keys
                                    .contains(&record.public_key.zei_to_bytes())
                                {
                                    return Err(eg!(inp_fail!()));
                                }
                                cosig_keys
                                    .entry((op_idx, input_idx))
                                    .or_insert_with(HashSet::new);
                            }

                            let policies = trn.body.policies.to_ref();
                            verify_xfr_body(
                                &mut prng,
                                &mut params,
                                &trn.body.transfer,
                                &policies,
                            )
                            .c(d!(PlatformError::ZeiError(None)))?;

                            // Track policies that each asset was validated under
                            for (input_policies, record) in trn
                                .body
                                .policies
                                .inputs_tracing_policies
                                .iter()
                                .zip(trn.body.transfer.inputs.iter())
                                .chain(
                                    trn.body
                                        .policies
                                        .outputs_tracing_policies
                                        .iter()
                                        .zip(trn.body.transfer.outputs.iter()),
                                )
                            {
                                // Only non-confidential assets can be traced
                                if let Some(inp_code) =
                                    record.asset_type.get_asset_type()
                                {
                                    let prev_policies = tracing_policies.insert(
                                        AssetTypeCode { val: inp_code },
                                        input_policies.clone(),
                                    );

                                    // Tracing policies must be consistent w.r.t asset type (cant change)
                                    if prev_policies.is_some()
                                        && prev_policies.c(d!())? != *input_policies
                                    {
                                        return Err(eg!(inp_fail!()));
                                    }
                                }
                            }
                        }
                    }
                    // (3)
                    let mut input_types = HashSet::new();
                    for ((inp, record), lien) in trn
                        .body
                        .inputs
                        .iter()
                        .zip(trn.body.transfer.inputs.iter())
                        .zip(lien_inputs)
                    {
                        // NOTE: We assume that any confidential-type asset records
                        // have no atypical transfer restrictions. Be careful!
                        if let Some(inp_code) = record.asset_type.get_asset_type() {
                            input_types.insert(AssetTypeCode { val: inp_code });
                            //asset_types_involved.insert(AssetTypeCode { val: inp_code });
                        }

                        // (2), checking within this transaction and recording
                        // external UTXOs
                        match *inp {
                            TxoRef::Relative(offs) => {
                                // (2).(a)
                                if offs as usize >= txo_count {
                                    return Err(eg!(inp_fail!()));
                                }
                                let ix = (txo_count - 1) - (offs as usize);
                                match &txos[ix] {
                                    None => {
                                        return Err(eg!(inp_fail!()));
                                    }
                                    Some(txo) => {
                                        // (2).(b)
                                        if &txo.record != record
                                            || txo.lien != lien.cloned()
                                        {
                                            return Err(eg!(inp_fail!()));
                                        }
                                        internally_spent_txos.push(txo.clone());
                                    }
                                }
                                txos[ix] = None;
                            }
                            TxoRef::Absolute(txo_sid) => {
                                // (2).(a), partially
                                if input_txos.contains_key(&txo_sid) {
                                    return Err(eg!(inp_fail!()));
                                }

                                input_txos.insert(
                                    txo_sid,
                                    TxOutput {
                                        id: None,
                                        record: record.clone(),
                                        lien: lien.cloned(),
                                    },
                                );
                            }
                        }
                    }

                    txos.reserve(trn.body.transfer.outputs.len());
                    let mut conf_transfer = false;
                    for (out, lien) in trn.body.transfer.outputs.iter().zip(lien_outputs)
                    {
                        if let XfrAssetType::Confidential(_) = out.asset_type {
                            conf_transfer = true;
                        }
                        if let Some(out_code) = out.asset_type.get_asset_type() {
                            asset_types_involved.insert(AssetTypeCode { val: out_code });
                        }
                        txos.push(Some(TxOutput {
                            id: None,
                            record: out.clone(),
                            lien: lien.cloned(),
                        }));
                        txo_count += 1;
                    }
                    // Until we can distinguish assets that have policies that invoke transfer restrictions
                    // from those that don't, make note of all non-confidential inputs of confidential
                    // transfers
                    asset_types_involved.extend(&input_types);
                    if conf_transfer {
                        confidential_transfer_inputs.extend(&input_types);
                    }
                }

                // An AIR assignment is valid iff:
                //     1)  The body signature is valid.
                //     2)  The credential commitment is valid for the public key of the signer.
                Operation::AIRAssign(air_assign) => {
                    let commitment = &air_assign.body.data;
                    let issuer_pk = &air_assign.body.issuer_pk;
                    let pok = &air_assign.body.pok;
                    let pk = &air_assign.pubkey;
                    if txn.body.no_replay_token != air_assign.body.no_replay_token {
                        return Err(eg!(inp_fail!(
                            "compute_effect: txn body token not equal to the token for this AIRAssign operation"
                        )));
                    }
                    // 1)
                    air_assign
                        .signature
                        .verify(&pk, &air_assign.body)
                        .c(d!(zei_fail!()))?;
                    // 2)
                    credential_verify_commitment(
                        issuer_pk,
                        &commitment,
                        pok,
                        pk.as_bytes(),
                    )
                    .c(d!(zei_fail!()))?;
                    air_updates.insert(
                        serde_json::to_string(&air_assign.body.addr).c(d!())?,
                        serde_json::to_string(&commitment).c(d!())?,
                    );
                }
                // A memo update is valid iff:
                // 1) The signature is valid.
                // 2) The asset type is updatable (checked later).
                // 3) The signing key is the asset issuer key (checked later).
                Operation::UpdateMemo(update_memo) => {
                    let pk = update_memo.pubkey;
                    if txn.body.no_replay_token != update_memo.body.no_replay_token {
                        return Err(eg!(inp_fail!(
                            "compute_effect: txn body token not equal to the token for this UpdateMemo operation"
                        )));
                    }
                    // 1)
                    update_memo
                        .signature
                        .verify(&pk, &update_memo.body)
                        .c(d!(zei_fail!()))?;

                    memo_updates.push((
                        update_memo.body.asset_type,
                        pk,
                        update_memo.body.new_memo.clone(),
                    ));
                }

                // A bind is valid iff:
                //     1) The signatures on the body (a) all are valid and (b)
                //        there is a signature for each non-custom-policy input key
                //          - Fully checked here
                //     2) (a) No input other than the contract input has a custom
                //        policy, and (b) the contract input does not have a
                //        confidential type
                //          - Partially checked here; as in Transfer, the exact
                //            policy won't be known until later.
                //     3) The UTXOs (a) exist on the ledger and (b) match the zei transaction.
                //          - Partially checked here -- anything which hasn't
                //            been checked will appear in `input_txos`
                //          - As in Transfer, we compute the complete TxOutputs
                //            from the XfrNote's inputs + the lien list
                //     4) The zei transaction is valid and has exactly one
                //        output (which matches the contract type).
                //          - Fully checked here
                //     5) The contract input does not already have a lien attached
                //          - Checked here (although the TxoSID <-> TxOutput match
                //            isn't checked until later)
                //
                Operation::BindAssets(bind_assets) => {
                    // contract + bound inputs
                    if 1 + bind_assets.body.inputs.len()
                        != bind_assets.body.transfer.inputs.len()
                    {
                        return Err(eg!(inp_fail!()));
                    }

                    // 1 output: the lien
                    if bind_assets.body.transfer.outputs.is_empty() {
                        return Err(eg!(inp_fail!()));
                    }
                    // // other outputs must have 0 amounts
                    // for o in bind_assets.body.transfer.outputs[1..].iter() {
                    //   if o.amount.get_amount() != Some(0) {
                    //     return Err(eg!(inp_fail!()));
                    //   }
                    // }

                    debug_assert!(
                        1 + bind_assets.body.inputs.len()
                            == bind_assets.body.transfer.inputs.len()
                    );

                    // 2(b)
                    if bind_assets.body.transfer.inputs[0]
                        .asset_type
                        .get_asset_type()
                        .is_none()
                        || bind_assets.body.transfer.inputs[0].asset_type
                            != bind_assets.body.transfer.outputs[0].asset_type
                    {
                        // dbg!(&bind_assets.body.transfer.inputs[0].asset_type);
                        // dbg!(&bind_assets.body.transfer.outputs[0].asset_type);
                        return Err(eg!(inp_fail!()));
                    }

                    // setup for (3)
                    // NOTE: `lien_inputs` includes `None` for the contract
                    let lien_inputs = {
                        let mut inps = bind_assets
                            .body
                            .transfer
                            .inputs
                            .iter()
                            .map(|_| None)
                            .collect::<Vec<_>>();
                        for (inp_ix, hash) in bind_assets.body.input_liens.iter() {
                            let inp_ix = 1 + *inp_ix;
                            match inps.get(inp_ix) {
                                Some(None) => {
                                    inps[inp_ix] = Some(hash);
                                }
                                _ => {
                                    return Err(eg!(inp_fail!()));
                                }
                            }
                        }
                        inps
                    };

                    debug_assert!(
                        lien_inputs.len() == bind_assets.body.transfer.inputs.len()
                    );

                    let mut input_keys = HashSet::new();
                    // (1a) all body signatures are valid
                    for sig in &bind_assets.body_signatures {
                        if !bind_assets.body.verify_body_signature(sig) {
                            return Err(eg!(inp_fail!()));
                        }
                        if let Some(input_idx) = sig.input_idx {
                            let sig_keys = cosig_keys
                                .entry((op_idx, input_idx))
                                .or_insert_with(HashSet::new);
                            (*sig_keys).insert(sig.address.key.zei_to_bytes());
                        } else {
                            input_keys.insert(sig.address.key.zei_to_bytes());
                        }
                    }

                    // (1b) all input record owners (for non-custom-policy
                    //      assets) have signed
                    for (input_idx, record) in
                        bind_assets.body.transfer.inputs.iter().enumerate()
                    {
                        // skip signature checking for custom-policy assets
                        if let Some(inp_code) = record.asset_type.get_asset_type() {
                            if custom_policy_asset_types
                                .get(&AssetTypeCode { val: inp_code })
                                .is_some()
                            {
                                // (2) (only contract is allowed a custom policy)
                                if input_idx != 0 {
                                    return Err(eg!(inp_fail!()));
                                }

                                continue;
                            }
                        }
                        if !input_keys.contains(&record.public_key.zei_to_bytes()) {
                            return Err(eg!(inp_fail!()));
                        }
                        cosig_keys
                            .entry((op_idx, input_idx))
                            .or_insert_with(HashSet::new);
                    }

                    // (3)
                    // TODO(joe): support identity tracing in Bind/Release
                    let policies = {
                        let num_inputs = bind_assets.body.transfer.inputs.len();
                        let num_outputs = bind_assets.body.transfer.outputs.len();
                        let no_policies = TracingPolicies::new();
                        XfrNotePolicies::new(
                            vec![no_policies.clone(); num_inputs],
                            vec![None; num_inputs],
                            vec![no_policies; num_outputs],
                            vec![None; num_outputs],
                        )
                    };
                    verify_xfr_body(
                        &mut prng,
                        &mut params,
                        &bind_assets.body.transfer,
                        &policies.to_ref(),
                    )
                    .c(d!(PlatformError::ZeiError(None)))?;

                    // The vec of TxOutputs corresponding to the lien
                    let bound_inputs = bind_assets
                        .body
                        .transfer
                        .inputs
                        .iter()
                        .zip(lien_inputs.iter())
                        // Skipping the contract
                        .skip(1)
                        .map(|(ar, lien)| TxOutput {
                            id: None,
                            record: ar.clone(),
                            lien: lien.cloned(),
                        })
                        .collect::<Vec<_>>();

                    // dbg!(&bound_inputs);

                    // (5) NOTE: the inputs are [contract] + inputs, and the contract is
                    // not allowed to have a lien bound to it already (hence
                    // once(None))
                    let mut input_types = HashSet::new();
                    for ((inp, record), lien) in once(&bind_assets.body.contract)
                        .chain(bind_assets.body.inputs.iter())
                        .zip(bind_assets.body.transfer.inputs.iter())
                        .zip(once(None).chain(lien_inputs.into_iter()))
                    {
                        // NOTE: We assume that any confidential-type asset records
                        // have no atypical transfer restrictions. Be careful!
                        if let Some(inp_code) = record.asset_type.get_asset_type() {
                            input_types.insert(AssetTypeCode { val: inp_code });
                            //asset_types_involved.insert(AssetTypeCode { val: inp_code });
                        }

                        // (2), checking within this transaction and recording
                        // external UTXOs
                        match *inp {
                            TxoRef::Relative(offs) => {
                                // (2).(a)
                                if offs as usize >= txo_count {
                                    return Err(eg!(inp_fail!()));
                                }
                                let ix = (txo_count - 1) - (offs as usize);
                                match &txos[ix] {
                                    None => {
                                        return Err(eg!(inp_fail!()));
                                    }
                                    Some(txo) => {
                                        let TxOutput {
                                            id: _,
                                            record: inp_record,
                                            lien: inp_lien,
                                        } = &txo;
                                        // (2).(b)
                                        if inp_record != record
                                            || inp_lien != &lien.cloned()
                                        {
                                            return Err(eg!(inp_fail!()));
                                        }
                                        internally_spent_txos.push(txo.clone());
                                    }
                                }
                                txos[ix] = None;
                            }
                            TxoRef::Absolute(txo_sid) => {
                                // (2).(a), partially
                                if input_txos.contains_key(&txo_sid) {
                                    return Err(eg!(inp_fail!()));
                                }

                                input_txos.insert(
                                    txo_sid,
                                    TxOutput {
                                        id: None,
                                        record: record.clone(),
                                        lien: lien.cloned(),
                                    },
                                );
                            }
                        }
                    }

                    // A bind has exactly one output: the lien
                    txos.reserve(1);
                    // assert_eq!(bind_assets.body.transfer.outputs.len(), 1);

                    {
                        assert_eq!(
                            bind_assets.body.transfer.inputs[0].asset_type,
                            bind_assets.body.transfer.outputs[0].asset_type
                        );
                        let (out, lien) = (
                            &bind_assets.body.transfer.outputs[0],
                            Some(HashOf::new(&bound_inputs)),
                        );
                        asset_types_involved.insert(AssetTypeCode {
                            val: out.asset_type.get_asset_type().c(d!())?,
                        });
                        txos.push(Some(TxOutput {
                            id: None,
                            record: out.clone(),
                            lien,
                        }));
                        txo_count += 1;
                    }

                    // Until we can distinguish assets that have policies that invoke transfer restrictions
                    // from those that don't, make note of all non-confidential inputs of confidential
                    // transfers
                    asset_types_involved.extend(&input_types);
                }

                // A release is valid iff:
                //     1) The signatures on the body (a) all are valid and (b)
                //        there is a signature for the contract input unless it has
                //        a custom policy
                //          - Fully checked here
                //     2) (a) The contract UTXO exists on the ledger, and (b)
                //        the zei transaction has the contract TXO and the bound
                //        UTXOs as inputs.
                //          - (a) is partially checked here (recorded in
                //            `input_txos`)
                //          - (b) is fully checked here, specifically that:
                //            - The inputs of the XfrNote are exactly
                //              [(contract,lien)] ++ [lien items which hash to `lien`]
                //     3) The zei transaction is valid.
                //          - Fully checked here
                //
                Operation::ReleaseAssets(release_assets) => {
                    if release_assets.body.transfer.inputs.is_empty() {
                        return Err(eg!(inp_fail!()));
                    }
                    if release_assets.body.num_outputs
                        != release_assets.body.transfer.outputs.len()
                    {
                        return Err(eg!(inp_fail!()));
                    }
                    debug_assert!(!release_assets.body.transfer.inputs.is_empty());
                    debug_assert!(
                        release_assets.body.num_outputs
                            == release_assets.body.transfer.outputs.len()
                    );

                    // setup for (2)
                    let (lien_inputs, lien_outputs) = {
                        let mut inps = release_assets
                            .body
                            .transfer
                            .inputs
                            .iter()
                            .map(|_| None)
                            .collect::<Vec<_>>();
                        let mut outs = release_assets
                            .body
                            .transfer
                            .outputs
                            .iter()
                            .map(|_| None)
                            .collect::<Vec<_>>();
                        for (inp_ix, out_ix, hash) in
                            release_assets.body.lien_assignments.iter()
                        {
                            let (inp_ix, out_ix) = (*inp_ix, *out_ix);
                            // The contract input is a special case
                            if inp_ix == 0 {
                                return Err(eg!(inp_fail!()));
                            }
                            match (inps.get(inp_ix), outs.get(out_ix)) {
                                (Some(None), Some(None)) => {
                                    inps[inp_ix] = Some(hash);
                                    outs[out_ix] = Some(hash);
                                }
                                _ => {
                                    return Err(eg!(inp_fail!()));
                                }
                            }
                        }
                        (inps, outs)
                    };

                    debug_assert!(
                        lien_inputs.len() == release_assets.body.transfer.inputs.len()
                    );

                    // TODO(joe): Can this safely be removed?
                    //  - It *SHOULD* be enforced by BindAssets validation
                    //  - If there's a bug in BindAssets validation that allows a
                    //    confidential asset type on the contract, what is the sane
                    //    behavior?
                    //  - Rejecting later transactions that use such a lien seems
                    //    like a "fail-safe" option -- is that right?
                    if release_assets.body.transfer.inputs[0]
                        .asset_type
                        .get_asset_type()
                        .is_none()
                    {
                        return Err(eg!(inp_fail!()));
                    }

                    let mut input_keys = HashSet::new();
                    // (1a) all body signatures are valid
                    for sig in &release_assets.body_signatures {
                        if !release_assets.body.verify_body_signature(sig) {
                            return Err(eg!(inp_fail!()));
                        }
                        if let Some(input_idx) = sig.input_idx {
                            let sig_keys = cosig_keys
                                .entry((op_idx, input_idx))
                                .or_insert_with(HashSet::new);
                            (*sig_keys).insert(sig.address.key.zei_to_bytes());
                        } else {
                            input_keys.insert(sig.address.key.zei_to_bytes());
                        }
                    }

                    // (1b) the contract input record owners (for non-custom-policy
                    //      assets) have signed
                    for (input_idx, record) in
                        once(&release_assets.body.transfer.inputs[0]).enumerate()
                    {
                        // skip signature checking for custom-policy assets
                        if let Some(inp_code) = record.asset_type.get_asset_type() {
                            if custom_policy_asset_types
                                .get(&AssetTypeCode { val: inp_code })
                                .is_some()
                            {
                                continue;
                            }
                        } else {
                            panic!("{}", inp_fail!());
                        }
                        if !input_keys.contains(&record.public_key.zei_to_bytes()) {
                            return Err(eg!(inp_fail!()));
                        }
                        cosig_keys
                            .entry((op_idx, input_idx))
                            .or_insert_with(HashSet::new);
                    }

                    // (3)
                    // TODO: implement real policies
                    let policies = {
                        let num_inputs = release_assets.body.transfer.inputs.len();
                        let num_outputs = release_assets.body.transfer.outputs.len();
                        let no_policies = TracingPolicies::new();
                        XfrNotePolicies::new(
                            vec![no_policies.clone(); num_inputs],
                            vec![None; num_inputs],
                            vec![no_policies; num_outputs],
                            vec![None; num_outputs],
                        )
                    };
                    verify_xfr_body(
                        &mut prng,
                        &mut params,
                        &release_assets.body.transfer,
                        &policies.to_ref(),
                    )
                    .c(d!(PlatformError::ZeiError(None)))?;

                    // The vec of TxOutputs corresponding to the lien
                    let bound_inputs = release_assets
                        .body
                        .transfer
                        .inputs
                        .iter()
                        .zip(lien_inputs.iter())
                        // Skipping the contract
                        .skip(1)
                        .map(|(ar, lien)| TxOutput {
                            id: None,
                            record: ar.clone(),
                            lien: lien.cloned(),
                        })
                        .collect::<Vec<_>>();
                    // dbg!(&bound_inputs);

                    let mut input_types = HashSet::new();
                    for record in release_assets.body.transfer.inputs.iter() {
                        // NOTE: We assume that any confidential-type asset records
                        // have no atypical transfer restrictions. Be careful!
                        if let Some(inp_code) = record.asset_type.get_asset_type() {
                            input_types.insert(AssetTypeCode { val: inp_code });
                            //asset_types_involved.insert(AssetTypeCode { val: inp_code });
                        }
                    }

                    {
                        let (inp, record) = (
                            &release_assets.body.contract,
                            &release_assets.body.transfer.inputs[0],
                        );
                        let lien = Some(HashOf::new(&bound_inputs));

                        // (2), checking within this transaction and recording
                        // external UTXOs
                        match *inp {
                            TxoRef::Relative(offs) => {
                                // (2).(a)
                                if offs as usize >= txo_count {
                                    return Err(eg!(inp_fail!()));
                                }
                                let ix = (txo_count - 1) - (offs as usize);
                                match &txos[ix] {
                                    None => {
                                        return Err(eg!(inp_fail!()));
                                    }
                                    Some(txo) => {
                                        let TxOutput {
                                            id: _,
                                            record: inp_record,
                                            lien: inp_lien,
                                        } = &txo;
                                        // (2).(b)
                                        if inp_record != record || inp_lien != &lien {
                                            return Err(eg!(inp_fail!()));
                                        }
                                        internally_spent_txos.push(txo.clone());
                                    }
                                }
                                txos[ix] = None;
                            }
                            TxoRef::Absolute(txo_sid) => {
                                // (2).(a), partially
                                if input_txos.contains_key(&txo_sid) {
                                    return Err(eg!(inp_fail!()));
                                }

                                input_txos.insert(
                                    txo_sid,
                                    TxOutput {
                                        id: None,
                                        record: record.clone(),
                                        lien,
                                    },
                                );
                            }
                        }
                    }

                    txos.reserve(release_assets.body.transfer.outputs.len());
                    let mut conf_transfer = false;
                    for (out, lien) in release_assets
                        .body
                        .transfer
                        .outputs
                        .iter()
                        .zip(lien_outputs)
                    {
                        if let XfrAssetType::Confidential(_) = out.asset_type {
                            conf_transfer = true;
                        }
                        if let Some(out_code) = out.asset_type.get_asset_type() {
                            asset_types_involved.insert(AssetTypeCode { val: out_code });
                        }
                        txos.push(Some(TxOutput {
                            id: None,
                            record: out.clone(),
                            lien: lien.cloned(),
                        }));
                        txo_count += 1;
                    }
                    // Until we can distinguish assets that have policies that invoke transfer restrictions
                    // from those that don't, make note of all non-confidential inputs of confidential
                    // transfers
                    asset_types_involved.extend(&input_types);
                    if conf_transfer {
                        confidential_transfer_inputs.extend(&input_types);
                    }
                }
            } // end -- match op {...}
            op_idx += 1;
        } // end -- for op in txn.body.operations.iter() {...}

        Ok(TxnEffect {
            txn,
            txos,
            input_txos,
            internally_spent_txos,
            new_asset_codes,
            new_issuance_nums,
            issuance_keys,
            issuance_amounts,
            confidential_issuance_types,
            cosig_keys,
            debt_effects,
            confidential_transfer_inputs,
            tracing_policies,
            asset_types_involved,
            custom_policy_asset_types,
            air_updates,
            kv_updates,
            memo_updates,
        })
    }
}

impl HasInvariants for TxnEffect {
    fn fast_invariant_check(&self) -> Result<()> {
        Ok(())
    }

    fn deep_invariant_check(&self) -> Result<()> {
        // Kinda messy, but the intention of this loop is to encode: For
        // every external input of a TxnEffect, there is exactly one
        // TransferAsset which consumes it.
        for (txo_sid, record) in self.input_txos.iter() {
            let mut found = false;
            for op in self.txn.body.operations.iter() {
                if let Operation::TransferAsset(trn) = op {
                    if trn.body.inputs.len() != trn.body.transfer.inputs.len() {
                        return Err(eg!(inv_fail!()));
                    }
                    for (ix, inp_record) in
                        trn.body.inputs.iter().zip(trn.body.transfer.inputs.iter())
                    {
                        if let TxoRef::Absolute(input_tid) = ix {
                            if input_tid == txo_sid {
                                if inp_record != &record.record {
                                    return Err(eg!(inv_fail!()));
                                }
                                if found {
                                    return Err(eg!(inv_fail!()));
                                }
                                found = true;
                            }
                        } else if inp_record == &record.record {
                            // TODO(joe): is this a reasonable check? I don't think anything
                            // guarantees that records are unique.
                            // Right now nothing calls this, but I think it's wrong...
                            return Err(eg!(inv_fail!()));
                        }
                    }
                }
            }
            if !found {
                return Err(eg!(inv_fail!()));
            }
        }

        // TODO(joe): Every Utxo corresponds to exactly one TranferAsset or
        // IssueAsset, and does not appear in any inputs

        // TODO(joe): other checks?
        {
            // Slightly cheating
            if TxnEffect::compute_effect(self.txn.clone()).c(d!())? != *self {
                return Err(eg!(inv_fail!()));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Default, Serialize)]
pub struct BlockEffect {
    // All Transaction objects validated in this block
    pub txns: Vec<Transaction>,
    // All NoReplayTokens seen in this block
    pub no_replay_tokens: Vec<NoReplayToken>,
    // Identifiers within this block for each transaction
    // (currently just an index into `txns`)
    pub temp_sids: Vec<TxnTempSID>,
    // Internally-spent TXOs are None, UTXOs are Some(...)
    // Should line up element-wise with `txns`
    pub txos: Vec<Vec<Option<TxOutput>>>,
    // Which TXOs this consumes
    pub input_txos: HashMap<TxoSID, TxOutput>,
    // Which new asset types this defines
    pub new_asset_codes: HashMap<AssetTypeCode, AssetType>,
    // Which new TXO issuance sequence numbers are used, in sorted order
    // The vec should be nonempty unless this asset code is being created in
    // this transaction.
    pub new_issuance_nums: HashMap<AssetTypeCode, Vec<u64>>,
    // New issuance amounts
    pub issuance_amounts: HashMap<AssetTypeCode, u64>,
    // Which public key is being used to issue each asset type
    pub issuance_keys: HashMap<AssetTypeCode, IssuerPublicKey>,
    // Updates to the AIR
    pub air_updates: HashMap<String, String>,
    // User-provided Key-Value store updates
    pub kv_updates: HashMap<Key, Vec<(KVEntrySignature, u64, Option<KVEntry>)>>,
    // Memo updates
    pub memo_updates: HashMap<AssetTypeCode, Memo>,
    // counter for consensus integration; will add to a running count when applied.
    pub pulse_count: u64,
}

impl BlockEffect {
    pub fn new() -> BlockEffect {
        Default::default()
    }

    // Combine a TxnEffect into this block.
    //
    // NOTE: this does not check the TxnEffect against the rest of the ledger
    // state, so each TxnEffect should be passed through
    // LedgerStatus::check_txn_effects *first*.
    //
    // Returns:
    //   if `txn` would not interfere with any transaction in the block, the
    //       new temp SID representing the transaction.
    //   Otherwise, Err(...)
    #[allow(clippy::cognitive_complexity)]
    pub fn add_txn_effect(&mut self, txn_effect: TxnEffect) -> Result<TxnTempSID> {
        // Check that KV updates are independent
        for (k, _) in txn_effect.kv_updates.iter() {
            if self.kv_updates.contains_key(&k) {
                return Err(eg!(PlatformError::InputsError(None)));
            }
        }

        // Check that no inputs are consumed twice
        for (input_sid, _) in txn_effect.input_txos.iter() {
            if self.input_txos.contains_key(&input_sid) {
                return Err(eg!(inp_fail!()));
            }
        }

        // Check that no AssetType is affected by both the block so far and
        // this transaction
        {
            for (type_code, _) in txn_effect.new_asset_codes.iter() {
                if self.new_asset_codes.contains_key(&type_code)
                    || self.new_issuance_nums.contains_key(&type_code)
                {
                    return Err(eg!(inp_fail!()));
                }
            }

            for (type_code, nums) in txn_effect.new_issuance_nums.iter() {
                if self.new_asset_codes.contains_key(&type_code)
                    || self.new_issuance_nums.contains_key(&type_code)
                {
                    return Err(eg!(inp_fail!()));
                }

                // Debug-check that issued assets are registered in `issuance_keys`
                if !nums.is_empty() {
                    debug_assert!(txn_effect.issuance_keys.contains_key(&type_code));
                }
            }
            // Ensure that each asset's memo can only be updated once per block
            for (type_code, _, _) in txn_effect.memo_updates.iter() {
                if self.memo_updates.contains_key(&type_code) {
                    return Err(eg!(inp_fail!()));
                }
            }
        }

        let no_replay_token = txn_effect.txn.body.no_replay_token;
        // Check that no operations are duplicated as in a replay attack
        // Note that we need to check here as well as in LedgerStatus::check_txn_effect
        for txn in self.txns.iter() {
            if txn.body.no_replay_token == no_replay_token {
                return Err(eg!(PlatformError::InputsError(None)));
            }
        }

        self.no_replay_tokens.push(no_replay_token); // By construction, no_replay_tokens entries are unique

        // == All validation done, apply `txn_effect` to this block ==
        for (k, update) in txn_effect.kv_updates {
            self.kv_updates.insert(k, update);
        }

        let temp_sid = TxnTempSID(self.txns.len());
        self.txns.push(txn_effect.txn);
        self.temp_sids.push(temp_sid);
        self.txos.push(txn_effect.txos);

        for (input_sid, record) in txn_effect.input_txos {
            // dbg!(&input_sid);
            debug_assert!(!self.input_txos.contains_key(&input_sid));
            self.input_txos.insert(input_sid, record);
        }

        for (type_code, asset_type) in txn_effect.new_asset_codes {
            debug_assert!(!self.new_asset_codes.contains_key(&type_code));
            self.new_asset_codes.insert(type_code, asset_type);
        }

        for (type_code, issuance_nums) in txn_effect.new_issuance_nums {
            debug_assert!(!self.new_issuance_nums.contains_key(&type_code));
            self.new_issuance_nums.insert(type_code, issuance_nums);
        }

        for (type_code, amount) in txn_effect.issuance_amounts.iter() {
            let issuance_amount = self.issuance_amounts.entry(*type_code).or_insert(0);
            *issuance_amount += amount;
        }

        for (addr, data) in txn_effect.air_updates {
            debug_assert!(!self.air_updates.contains_key(&addr));
            self.air_updates.insert(addr, data);
        }

        for (code, _, memo) in txn_effect.memo_updates {
            self.memo_updates.insert(code, memo);
        }

        Ok(temp_sid)
    }

    pub fn get_pulse_count(&self) -> u64 {
        self.pulse_count
    }

    pub fn add_pulse(&mut self) -> u64 {
        self.pulse_count += 1;
        self.pulse_count
    }
    pub fn compute_txns_in_block_hash(&self) -> HashOf<Vec<Transaction>> {
        HashOf::new(&self.txns)
    }
}