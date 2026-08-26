//! # AstraPort Versioning Contract
//!
//! Contract upgrade and versioning system for the AstraPort protocol. Provides:
//!
//! - **Version tracking** — queryable metadata for every deployed version.
//! - **Multi-sig upgrade authorization** — proposals require N-of-M approvals.
//! - **Migration logic** — ordered migration steps between versions with
//!   backward-compatibility checks.
//! - **Feature flags** — gradual-rollout controls tied to version requirements.
//! - **Rollback** — restore a previous version when an upgrade fails.
//! - **Frozen versions** — lock old versions for archival.
//! - **Audit trail** — immutable log of every version change.
//!
//! ## Module overview
//!
//! - [`records`] — types: `VersionMetadata`, `UpgradeProposal`, `FeatureFlag`,
//!   `MigrationRecord`, `VersionAuditEntry`, `VersionStorageKey`.
//! - [`migration`] — `check_compatibility`, `execute_migration`,
//!   `execute_rollback`.
//!
//! ## Lifecycle
//!
//! 1. `initialize(admin)` once on deployment.
//! 2. `add_version(...)` to register a new contract version (admin-only).
//! 3. `propose_upgrade(...)` to propose activating a version (admin-only).
//! 4. `approve_upgrade(...)` by registered signers until threshold is met.
//! 5. `execute_upgrade(...)` to finalize the upgrade (any signer once approved).
//! 6. `set_feature_flag(...)` / `get_feature_flag(...)` for feature control.
//! 7. `rollback(...)` to revert a failed upgrade.

#![no_std]
#![allow(clippy::too_many_arguments)]

extern crate alloc;

use soroban_sdk::{
    contract, contracterror, contractimpl, symbol_short, Address, BytesN, Env, Symbol, Vec,
};

pub mod migration;
pub mod records;

use crate::migration::{execute_migration, execute_rollback};
use crate::records::{
    FeatureFlag, FeatureFlagStatus, MigrationRecord, UpgradeProposal, VersionAuditEntry,
    VersionMetadata, VersionStatus, VersionStorageKey,
};

// ---------------------------------------------------------------------------
// Sentinel
// ---------------------------------------------------------------------------

const OK: Symbol = symbol_short!("ok");

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by the versioning contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// Contract was not initialized.
    NotInitialized = 1,
    /// Caller is not the admin.
    Unauthorized = 2,
    /// The version already exists.
    VersionAlreadyExists = 3,
    /// No version has been registered yet.
    NoVersions = 4,
    /// The proposal was not found.
    ProposalNotFound = 5,
    /// The signer has already approved this proposal.
    AlreadyApproved = 6,
    /// The signer is not a registered multi-sig signer.
    NotASigner = 7,
    /// The proposal has already been executed.
    ProposalAlreadyExecuted = 8,
    /// The proposal was rejected.
    ProposalRejected = 9,
    /// The proposal has not reached the approval threshold.
    InsufficientApprovals = 10,
    /// The feature flag already exists.
    FlagAlreadyExists = 11,
    /// The feature flag was not found.
    FlagNotFound = 12,
    /// Cannot freeze the currently active version.
    CannotFreezeActiveVersion = 13,
    /// Version is already frozen.
    AlreadyFrozen = 14,
    /// Invalid rollout percentage (must be 0–100).
    InvalidRolloutPercentage = 15,
    /// Migration between these versions has already been recorded.
    MigrationAlreadyRecorded = 16,
    /// Contract was initialized previously.
    AlreadyInitialized = 17,
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

/// Versioning contract for AstraPort.
#[contract]
pub struct VersioningContract;

#[contractimpl]
impl VersioningContract {
    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    /// Initialize the versioning contract with an admin address.
    ///
    /// Sets the approval threshold to 1 (single-admin by default). Additional
    /// signers and a higher threshold should be configured afterward via
    /// `add_signer` and `set_approval_threshold`.
    pub fn initialize(env: Env, admin: Address) -> Result<Symbol, Error> {
        if env.storage().persistent().has(&VersionStorageKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage()
            .persistent()
            .set(&VersionStorageKey::Admin, &admin);
        env.storage()
            .persistent()
            .set(&VersionStorageKey::ApprovalThreshold, &1u32);
        env.storage()
            .persistent()
            .set(&VersionStorageKey::CurrentVersion, &0u32);
        env.storage()
            .persistent()
            .set(&VersionStorageKey::NextProposalId, &1u64);
        env.storage()
            .persistent()
            .set(&VersionStorageKey::NextAuditSeq, &1u64);
        env.storage()
            .persistent()
            .set(&VersionStorageKey::AllVersions, &Vec::<u32>::new(&env));
        env.storage()
            .persistent()
            .set(&VersionStorageKey::Signers, &Vec::<Address>::new(&env));
        env.storage().persistent().set(
            &VersionStorageKey::AllFeatureFlags,
            &Vec::<Symbol>::new(&env),
        );
        env.storage()
            .persistent()
            .set(&VersionStorageKey::FrozenVersions, &Vec::<u32>::new(&env));
        Ok(OK)
    }

    /// Return the current admin address.
    pub fn get_admin(env: Env) -> Result<Address, Error> {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::Admin)
            .ok_or(Error::NotInitialized)
    }

    // -----------------------------------------------------------------------
    // Multi-sig management
    // -----------------------------------------------------------------------

    /// Add a signer to the multi-sig set. Admin-only.
    pub fn add_signer(env: Env, admin: Address, signer: Address) -> Result<Symbol, Error> {
        Self::assert_admin(&env, &admin)?;

        let mut signers: Vec<Address> = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::Signers)
            .unwrap_or_else(|| Vec::new(&env));

        // Prevent duplicates without relying on host-side vector search.
        for existing in signers.iter() {
            if existing == signer {
                return Ok(OK);
            }
        }

        signers.push_back(signer);
        env.storage()
            .persistent()
            .set(&VersionStorageKey::Signers, &signers);
        Ok(OK)
    }

    /// Remove a signer. Admin-only.
    pub fn remove_signer(env: Env, admin: Address, signer: Address) -> Result<Symbol, Error> {
        Self::assert_admin(&env, &admin)?;

        let signers: Vec<Address> = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::Signers)
            .unwrap_or_else(|| Vec::new(&env));

        let mut remaining = Vec::new(&env);
        for existing in signers.iter() {
            if existing != signer {
                remaining.push_back(existing);
            }
        }
        env.storage()
            .persistent()
            .set(&VersionStorageKey::Signers, &remaining);

        Ok(OK)
    }

    /// Set the number of approvals required to execute an upgrade. Admin-only.
    pub fn set_approval_threshold(
        env: Env,
        admin: Address,
        threshold: u32,
    ) -> Result<Symbol, Error> {
        Self::assert_admin(&env, &admin)?;

        if threshold == 0 {
            return Err(Error::InsufficientApprovals);
        }

        let signers: Vec<Address> = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::Signers)
            .unwrap_or_else(|| Vec::new(&env));

        // Threshold cannot exceed the number of registered signers + 1 (admin).
        let total_authorities = signers.len() + 1;
        if threshold > total_authorities {
            return Err(Error::InsufficientApprovals);
        }

        env.storage()
            .persistent()
            .set(&VersionStorageKey::ApprovalThreshold, &threshold);
        Ok(OK)
    }

    /// Get the current approval threshold.
    pub fn get_approval_threshold(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::ApprovalThreshold)
            .unwrap_or(1)
    }

    /// Get the list of registered signers.
    pub fn get_signers(env: Env) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::Signers)
            .unwrap_or_else(|| Vec::new(&env))
    }

    // -----------------------------------------------------------------------
    // Version management
    // -----------------------------------------------------------------------

    /// Register a new contract version. Admin-only.
    ///
    /// The new version is stored with status `Proposed`. The caller must
    /// subsequently propose and approve an upgrade to make it active.
    pub fn add_version(
        env: Env,
        admin: Address,
        semantic_version: Symbol,
        wasm_hash: BytesN<32>,
        migration_steps: Vec<Symbol>,
        description: Symbol,
    ) -> Result<u32, Error> {
        Self::assert_admin(&env, &admin)?;

        let all_versions: Vec<u32> = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::AllVersions)
            .unwrap_or_else(|| Vec::new(&env));

        let version_number = all_versions.len() + 1;

        let now = env.ledger().timestamp();
        let meta = VersionMetadata {
            version_number,
            semantic_version,
            status: VersionStatus::Proposed,
            proposer: admin.clone(),
            proposed_at: now,
            activated_at: 0,
            wasm_hash,
            migration_steps,
            description,
        };

        env.storage()
            .persistent()
            .set(&VersionStorageKey::VersionMetadata(version_number), &meta);

        let mut versions = all_versions;
        versions.push_back(version_number);
        env.storage()
            .persistent()
            .set(&VersionStorageKey::AllVersions, &versions);

        // Audit trail.
        Self::append_audit(
            &env,
            &symbol_short!("add_ver"),
            version_number,
            &admin,
            &symbol_short!("new_ver"),
        );

        Ok(version_number)
    }

    /// Get metadata for a specific version.
    pub fn get_version_metadata(env: Env, version_number: u32) -> Option<VersionMetadata> {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::VersionMetadata(version_number))
    }

    /// Get the current active version number.
    pub fn get_current_version(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::CurrentVersion)
            .unwrap_or(0)
    }

    /// Get all registered version numbers.
    pub fn get_all_versions(env: Env) -> Vec<u32> {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::AllVersions)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get the count of registered versions.
    pub fn get_version_count(env: Env) -> u32 {
        let all: Vec<u32> = Self::get_all_versions(env);
        all.len()
    }

    // -----------------------------------------------------------------------
    // Upgrade proposal & approval
    // -----------------------------------------------------------------------

    /// Propose upgrading to a specific version. Admin-only.
    ///
    /// Creates a new [`UpgradeProposal`] and returns its id.
    pub fn propose_upgrade(env: Env, admin: Address, target_version: u32) -> Result<u64, Error> {
        Self::assert_admin(&env, &admin)?;

        // Validate the target version exists.
        let meta: Option<VersionMetadata> = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::VersionMetadata(target_version));
        if meta.is_none() {
            return Err(Error::ProposalNotFound);
        }

        let proposal_id: u64 = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::NextProposalId)
            .unwrap_or(1);

        let now = env.ledger().timestamp();
        let proposal = UpgradeProposal {
            proposal_id,
            target_version,
            proposer: admin.clone(),
            created_at: now,
            approvals: {
                let mut approvals = Vec::new(&env);
                approvals.push_back(admin.clone());
                approvals
            },
            executed: false,
            rejected: false,
        };

        env.storage()
            .persistent()
            .set(&VersionStorageKey::Proposal(proposal_id), &proposal);

        env.storage()
            .persistent()
            .set(&VersionStorageKey::NextProposalId, &(proposal_id + 1));

        Self::append_audit(
            &env,
            &symbol_short!("propose"),
            target_version,
            &admin,
            &symbol_short!("upgrade"),
        );

        Ok(proposal_id)
    }

    /// Approve an upgrade proposal. Must be called by a registered signer.
    pub fn approve_upgrade(env: Env, signer: Address, proposal_id: u64) -> Result<Symbol, Error> {
        signer.require_auth();

        // Verify signer is authorized.
        let signers: Vec<Address> = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::Signers)
            .unwrap_or_else(|| Vec::new(&env));
        let is_signer = signers.contains(&signer);
        let admin: Address = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::Admin)
            .ok_or(Error::NotInitialized)?;
        let is_admin = signer == admin;

        if !is_signer && !is_admin {
            return Err(Error::NotASigner);
        }

        let mut proposal: UpgradeProposal = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)?;

        if proposal.executed {
            return Err(Error::ProposalAlreadyExecuted);
        }
        if proposal.rejected {
            return Err(Error::ProposalRejected);
        }
        if proposal.approvals.contains(&signer) {
            return Err(Error::AlreadyApproved);
        }

        proposal.approvals.push_back(signer.clone());
        env.storage()
            .persistent()
            .set(&VersionStorageKey::Proposal(proposal_id), &proposal);

        Self::append_audit(
            &env,
            &symbol_short!("approve"),
            proposal.target_version,
            &signer,
            &symbol_short!("approval"),
        );

        Ok(OK)
    }

    /// Execute an upgrade proposal once enough approvals have been gathered.
    ///
    /// Any address may call this (the threshold is checked against the
    /// recorded approvals). The actual WASM upgrade is handled externally;
    /// this entrypoint performs the version state transition.
    pub fn execute_upgrade(env: Env, proposal_id: u64) -> Result<Symbol, Error> {
        let mut proposal: UpgradeProposal = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)?;

        if proposal.executed {
            return Err(Error::ProposalAlreadyExecuted);
        }
        if proposal.rejected {
            return Err(Error::ProposalRejected);
        }

        let threshold: u32 = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::ApprovalThreshold)
            .unwrap_or(1);

        if proposal.approvals.len() < threshold {
            return Err(Error::InsufficientApprovals);
        }

        let current_version: u32 = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::CurrentVersion)
            .unwrap_or(0);

        // Execute the migration.
        let caller = env.current_contract_address();
        let _record: MigrationRecord =
            execute_migration(&env, current_version, proposal.target_version, caller)
                .map_err(|_| Error::InsufficientApprovals)?;

        // Mark proposal as executed.
        proposal.executed = true;
        env.storage()
            .persistent()
            .set(&VersionStorageKey::Proposal(proposal_id), &proposal);

        Self::append_audit(
            &env,
            &symbol_short!("upgrade"),
            proposal.target_version,
            &proposal.proposer,
            &symbol_short!("executed"),
        );

        Ok(OK)
    }

    /// Reject an upgrade proposal. Admin-only.
    pub fn reject_upgrade(env: Env, admin: Address, proposal_id: u64) -> Result<Symbol, Error> {
        Self::assert_admin(&env, &admin)?;

        let mut proposal: UpgradeProposal = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)?;

        if proposal.executed {
            return Err(Error::ProposalAlreadyExecuted);
        }

        proposal.rejected = true;
        env.storage()
            .persistent()
            .set(&VersionStorageKey::Proposal(proposal_id), &proposal);

        Self::append_audit(
            &env,
            &symbol_short!("reject"),
            proposal.target_version,
            &admin,
            &symbol_short!("rejected"),
        );

        Ok(OK)
    }

    /// Get an upgrade proposal by id.
    pub fn get_proposal(env: Env, proposal_id: u64) -> Option<UpgradeProposal> {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::Proposal(proposal_id))
    }

    /// Check whether a proposal has reached the approval threshold.
    pub fn is_proposal_approved(env: Env, proposal_id: u64) -> bool {
        let proposal: Option<UpgradeProposal> = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::Proposal(proposal_id));
        match proposal {
            Some(p) => {
                let threshold: u32 = env
                    .storage()
                    .persistent()
                    .get(&VersionStorageKey::ApprovalThreshold)
                    .unwrap_or(1);
                p.approvals.len() >= threshold && !p.executed && !p.rejected
            }
            None => false,
        }
    }

    // -----------------------------------------------------------------------
    // Rollback
    // -----------------------------------------------------------------------

    /// Roll back from the current version to a specific previous version.
    ///
    /// Admin-only. The target version must have been the version immediately
    /// before the current one (status `Superseded`).
    pub fn rollback(env: Env, admin: Address, target_version: u32) -> Result<Symbol, Error> {
        Self::assert_admin(&env, &admin)?;

        let current_version: u32 = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::CurrentVersion)
            .unwrap_or(0);

        if current_version == 0 {
            return Err(Error::NoVersions);
        }

        let _record: MigrationRecord =
            execute_rollback(&env, current_version, target_version, admin.clone()).map_err(
                |e| {
                    // Map migration errors to versioning errors.
                    match e {
                        migration::MigrationError::TargetVersionNotFound => Error::ProposalNotFound,
                        migration::MigrationError::InvalidTargetStatus => Error::Unauthorized,
                        _ => Error::Unauthorized,
                    }
                },
            )?;

        Self::append_audit(
            &env,
            &symbol_short!("rollback"),
            target_version,
            &admin,
            &symbol_short!("reverted"),
        );

        Ok(OK)
    }

    // -----------------------------------------------------------------------
    // Feature flags
    // -----------------------------------------------------------------------

    /// Set (create or update) a feature flag. Admin-only.
    pub fn set_feature_flag(
        env: Env,
        admin: Address,
        flag_name: Symbol,
        status: FeatureFlagStatus,
        rollout_percentage: u32,
        min_version: u32,
        description: Symbol,
    ) -> Result<Symbol, Error> {
        Self::assert_admin(&env, &admin)?;

        if status == FeatureFlagStatus::GradualRollout && rollout_percentage > 100 {
            return Err(Error::InvalidRolloutPercentage);
        }

        let all_flags: Vec<Symbol> = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::AllFeatureFlags)
            .unwrap_or_else(|| Vec::new(&env));

        let now = env.ledger().timestamp();
        let flag = FeatureFlag {
            flag_name: flag_name.clone(),
            status,
            rollout_percentage,
            min_version,
            description,
            last_modified: now,
        };

        env.storage()
            .persistent()
            .set(&VersionStorageKey::FeatureFlag(flag_name.clone()), &flag);

        // Add to registry if new.
        if !all_flags.contains(&flag_name) {
            let mut flags = all_flags;
            flags.push_back(flag_name.clone());
            env.storage()
                .persistent()
                .set(&VersionStorageKey::AllFeatureFlags, &flags);
        }

        Self::append_audit(&env, &symbol_short!("set_flag"), 0, &admin, &flag_name);

        Ok(OK)
    }

    /// Get a feature flag by name.
    pub fn get_feature_flag(env: Env, flag_name: Symbol) -> Option<FeatureFlag> {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::FeatureFlag(flag_name))
    }

    /// Check if a feature flag is active for a given version and user index.
    ///
    /// - If the flag is `Enabled`, returns `true` for all versions >= min_version.
    /// - If the flag is `Disabled`, returns `false`.
    /// - If the flag is `GradualRollout`, returns `true` if
    ///   `(user_index % 100) < rollout_percentage` (deterministic per-user).
    /// - Also checks that `current_version >= min_version`.
    pub fn is_feature_enabled(
        env: Env,
        flag_name: Symbol,
        current_version: u32,
        user_index: u32,
    ) -> bool {
        let flag: FeatureFlag = match env
            .storage()
            .persistent()
            .get(&VersionStorageKey::FeatureFlag(flag_name))
        {
            Some(f) => f,
            None => return false,
        };

        // Version gate.
        if current_version < flag.min_version {
            return false;
        }

        match flag.status {
            FeatureFlagStatus::Enabled => true,
            FeatureFlagStatus::Disabled => false,
            FeatureFlagStatus::GradualRollout => {
                // Deterministic per-user: if user_index mod 100 < rollout_percentage.
                (user_index % 100) < flag.rollout_percentage
            }
        }
    }

    /// Get all registered feature flag names.
    pub fn get_all_feature_flags(env: Env) -> Vec<Symbol> {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::AllFeatureFlags)
            .unwrap_or_else(|| Vec::new(&env))
    }

    // -----------------------------------------------------------------------
    // Frozen versions
    // -----------------------------------------------------------------------

    /// Freeze a version for archival. Frozen versions cannot be activated.
    /// Admin-only. Cannot freeze the currently active version.
    pub fn freeze_version(env: Env, admin: Address, version_number: u32) -> Result<Symbol, Error> {
        Self::assert_admin(&env, &admin)?;

        let mut meta: VersionMetadata = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::VersionMetadata(version_number))
            .ok_or(Error::ProposalNotFound)?;

        let current: u32 = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::CurrentVersion)
            .unwrap_or(0);
        if version_number == current {
            return Err(Error::CannotFreezeActiveVersion);
        }

        if meta.status == VersionStatus::Frozen {
            return Err(Error::AlreadyFrozen);
        }

        meta.status = VersionStatus::Frozen;
        env.storage()
            .persistent()
            .set(&VersionStorageKey::VersionMetadata(version_number), &meta);

        // Track in frozen list.
        let mut frozen: Vec<u32> = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::FrozenVersions)
            .unwrap_or_else(|| Vec::new(&env));
        frozen.push_back(version_number);
        env.storage()
            .persistent()
            .set(&VersionStorageKey::FrozenVersions, &frozen);

        Self::append_audit(
            &env,
            &symbol_short!("freeze"),
            version_number,
            &admin,
            &symbol_short!("frozen"),
        );

        Ok(OK)
    }

    /// Get all frozen version numbers.
    pub fn get_frozen_versions(env: Env) -> Vec<u32> {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::FrozenVersions)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Check if a version is frozen.
    pub fn is_version_frozen(env: Env, version_number: u32) -> bool {
        let frozen: Vec<u32> = Self::get_frozen_versions(env);
        frozen.contains(version_number)
    }

    // -----------------------------------------------------------------------
    // Migration records
    // -----------------------------------------------------------------------

    /// Get the migration record for a specific version transition.
    pub fn get_migration_record(
        env: Env,
        from_version: u32,
        to_version: u32,
    ) -> Option<MigrationRecord> {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::MigrationRecord(
                from_version,
                to_version,
            ))
    }

    // -----------------------------------------------------------------------
    // Backward compatibility
    // -----------------------------------------------------------------------

    /// Check if upgrading from one version to another is compatible.
    ///
    /// Returns `true` if the upgrade path is valid.
    pub fn check_backward_compatibility(env: Env, from_version: u32, to_version: u32) -> bool {
        migration::check_compatibility(&env, from_version, to_version).is_ok()
    }

    // -----------------------------------------------------------------------
    // Audit trail
    // -----------------------------------------------------------------------

    /// Append an entry to the version audit trail.
    fn append_audit(
        env: &Env,
        action: &Symbol,
        version_number: u32,
        actor: &Address,
        detail: &Symbol,
    ) {
        let seq: u64 = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::NextAuditSeq)
            .unwrap_or(1);

        let entry = VersionAuditEntry {
            seq,
            timestamp: env.ledger().timestamp(),
            action: action.clone(),
            version_number,
            actor: actor.clone(),
            detail: detail.clone(),
        };

        let mut trail: Vec<VersionAuditEntry> = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::AuditTrail)
            .unwrap_or_else(|| Vec::new(env));
        trail.push_back(entry);
        env.storage()
            .persistent()
            .set(&VersionStorageKey::AuditTrail, &trail);
        env.storage()
            .persistent()
            .set(&VersionStorageKey::NextAuditSeq, &(seq + 1));
    }

    /// Get the full audit trail.
    pub fn get_audit_trail(env: Env) -> Vec<VersionAuditEntry> {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::AuditTrail)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get audit trail entries filtered by version number.
    pub fn get_audit_trail_for_version(env: Env, version_number: u32) -> Vec<VersionAuditEntry> {
        let trail: Vec<VersionAuditEntry> = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::AuditTrail)
            .unwrap_or_else(|| Vec::new(&env));

        let mut filtered = Vec::new(&env);
        for entry in trail.iter() {
            if entry.version_number == version_number {
                filtered.push_back(entry);
            }
        }
        filtered
    }

    // -----------------------------------------------------------------------
    // Audit-log sink (cross-contract integration)
    // -----------------------------------------------------------------------

    /// Configure the audit-log sink address. Admin-only.
    pub fn set_audit_sink(env: Env, admin: Address, sink: Address) -> Result<Symbol, Error> {
        Self::assert_admin(&env, &admin)?;
        env.storage()
            .persistent()
            .set(&VersionStorageKey::AuditSink, &sink);
        Ok(OK)
    }

    /// Read the audit-log sink address.
    pub fn get_audit_sink(env: Env) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&VersionStorageKey::AuditSink)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

impl VersioningContract {
    fn assert_admin(env: &Env, admin: &Address) -> Result<(), Error> {
        let stored: Address = env
            .storage()
            .persistent()
            .get(&VersionStorageKey::Admin)
            .ok_or(Error::NotInitialized)?;
        if *admin != stored {
            return Err(Error::Unauthorized);
        }
        admin.require_auth();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
