// Copyright (C) 2019-2023 Aleo Systems Inc.
// This file is part of the snarkVM library.

// The snarkVM library is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// The snarkVM library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with the snarkVM library. If not, see <https://www.gnu.org/licenses/>.

use crate::{
    atomic_write_batch,
    cow_to_cloned,
    cow_to_copied,
    store::helpers::{memory_map::MemoryMap, Map, MapRead},
};
use console::{
    network::{prelude::*, BHPMerkleTree},
    program::{Identifier, Plaintext, ProgramID, Value},
    types::Field,
};

use anyhow::Result;
use core::marker::PhantomData;
use indexmap::{IndexMap, IndexSet};
use parking_lot::RwLock;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[cfg(not(feature = "serial"))]
use rayon::prelude::*;

/// The depth of the finalize tree. See `FinalizeTree` for a description of the Merkle tree.
pub const FINALIZE_TREE_DEPTH: u8 = 32;
/// The depth of the program tree. See `ProgramTree` for a description of the Merkle tree.
pub const PROGRAM_TREE_DEPTH: u8 = 5;
/// The depth of the mapping tree. See `MappingTree` for a description of the Merkle tree.
pub const MAPPING_TREE_DEPTH: u8 = 32;

/// The Merkle tree that indexes all program trees.
/// Each leaf contains the Merkle root for a program tree.
pub type FinalizeTree<N> = BHPMerkleTree<N, FINALIZE_TREE_DEPTH>;
/// The Merkle tree for a program that contains a finalize scope.
/// Each leaf contains a Merkle root for a mapping tree.
pub type ProgramTree<N> = BHPMerkleTree<N, PROGRAM_TREE_DEPTH>;
/// The Merkle tree for a mapping within a program.
/// Each leaf contains the hash of a key-value entry.
pub type MappingTree<N> = BHPMerkleTree<N, MAPPING_TREE_DEPTH>;

/// Enum to represent the allowed set of Merkle tree operations.
#[derive(Clone, Copy, Debug)]
pub enum MerkleTreeUpdate<N: Network> {
    /// Insert a leaf into the tree, as (`mapping ID`, `key ID`, `value ID`).
    InsertValue(Field<N>, Field<N>, Field<N>),
    /// Update the leaf at the given index, as (`mapping ID`, `index`, `key ID`, `value ID`).
    UpdateValue(Field<N>, usize, Field<N>, Field<N>),
    /// Remove the leaf at the given index, as (`mapping ID`, `index`).
    RemoveValue(Field<N>, usize),
    /// Add the mapping to the tree, as (`mapping ID`).
    InsertMapping(Field<N>),
    /// Remove the mapping from the tree, as (`mapping ID`).
    RemoveMapping(Field<N>),
}

impl<N: Network> MerkleTreeUpdate<N> {
    /// Returns the mapping ID.
    pub fn mapping_id(&self) -> Field<N> {
        match self {
            MerkleTreeUpdate::InsertValue(mapping_id, _, _) => *mapping_id,
            MerkleTreeUpdate::UpdateValue(mapping_id, _, _, _) => *mapping_id,
            MerkleTreeUpdate::RemoveValue(mapping_id, _) => *mapping_id,
            MerkleTreeUpdate::InsertMapping(mapping_id) => *mapping_id,
            MerkleTreeUpdate::RemoveMapping(mapping_id) => *mapping_id,
        }
    }

    /// Returns the key ID if it exists.
    pub fn key_id(&self) -> Option<Field<N>> {
        match self {
            MerkleTreeUpdate::InsertValue(_, key_id, _) => Some(*key_id),
            MerkleTreeUpdate::UpdateValue(_, _, key_id, _) => Some(*key_id),
            MerkleTreeUpdate::RemoveValue(_, _) => None,
            MerkleTreeUpdate::InsertMapping(_) => None,
            MerkleTreeUpdate::RemoveMapping(_) => None,
        }
    }

    /// Returns `true` if the update is an `InsertValue`
    pub fn is_insert_value(&self) -> bool {
        matches!(self, MerkleTreeUpdate::InsertValue(_, _, _))
    }

    /// Returns `true` if the update is an `UpdateValue`
    pub fn is_update_value(&self) -> bool {
        matches!(self, MerkleTreeUpdate::UpdateValue(_, _, _, _))
    }

    /// Returns `true` if the update is a `RemoveValue`
    pub fn is_remove_value(&self) -> bool {
        matches!(self, MerkleTreeUpdate::RemoveValue(_, _))
    }

    /// Returns `true` if the update is an `InsertMapping`
    pub fn is_insert_mapping(&self) -> bool {
        matches!(self, MerkleTreeUpdate::InsertMapping(_))
    }

    /// Returns `true` if the update is a `RemoveMapping`
    pub fn is_remove_mapping(&self) -> bool {
        matches!(self, MerkleTreeUpdate::RemoveMapping(_))
    }
}

/// A trait for program state storage. Note: For the program logic, see `DeploymentStorage`.
///
/// We define the `mapping ID := Hash( program ID || mapping name )`,
/// and the `key ID := Hash ( mapping ID || Hash(key) )`,
/// and the `value ID := Hash ( key ID || Hash(value) )`.
///
/// `FinalizeStorage` emulates the following data structure:
/// ```text
/// // (program_id => (mapping_name => (key => value)))
/// BTreeMap<ProgramID<N>, BTreeMap<Identifier<N>, BTreeMap<Key, Value>>>
/// ```
pub trait FinalizeStorage<N: Network>: 'static + Clone + Send + Sync {
    /// The mapping of `program ID` to `[mapping name]`.
    type ProgramIDMap: for<'a> Map<'a, ProgramID<N>, IndexSet<Identifier<N>>>;
    /// The mapping of `program ID` to `deployment index`.
    type ProgramIndexMap: for<'a> Map<'a, ProgramID<N>, u32>;
    /// The mapping of `(program ID, mapping name)` to `mapping ID`.
    type MappingIDMap: for<'a> Map<'a, (ProgramID<N>, Identifier<N>), Field<N>>;
    /// The mapping of `mapping ID` to `[(key ID, value ID)]`.
    type KeyValueIDMap: for<'a> Map<'a, Field<N>, IndexMap<Field<N>, Field<N>>>;
    /// The mapping of `key ID` to `key`.
    type KeyMap: for<'a> Map<'a, Field<N>, Plaintext<N>>;
    /// The mapping of `key ID` to `value`.
    type ValueMap: for<'a> Map<'a, Field<N>, Value<N>>;

    /// Initializes the program state storage.
    fn open(dev: Option<u16>) -> Result<Self>;

    /// Returns the program ID map.
    fn program_id_map(&self) -> &Self::ProgramIDMap;
    /// Returns the program ID map.
    fn program_index_map(&self) -> &Self::ProgramIndexMap;
    /// Returns the mapping ID map.
    fn mapping_id_map(&self) -> &Self::MappingIDMap;
    /// Returns the key-value ID map.
    fn key_value_id_map(&self) -> &Self::KeyValueIDMap;
    /// Returns the key map.
    fn key_map(&self) -> &Self::KeyMap;
    /// Returns the value map.
    fn value_map(&self) -> &Self::ValueMap;

    /// Returns the optional development ID.
    fn dev(&self) -> Option<u16>;

    /// Starts an atomic batch write operation.
    fn start_atomic(&self) {
        self.program_id_map().start_atomic();
        self.program_index_map().start_atomic();
        self.mapping_id_map().start_atomic();
        self.key_value_id_map().start_atomic();
        self.key_map().start_atomic();
        self.value_map().start_atomic();
    }

    /// Checks if an atomic batch is in progress.
    fn is_atomic_in_progress(&self) -> bool {
        self.program_id_map().is_atomic_in_progress()
            || self.program_index_map().is_atomic_in_progress()
            || self.mapping_id_map().is_atomic_in_progress()
            || self.key_value_id_map().is_atomic_in_progress()
            || self.key_map().is_atomic_in_progress()
            || self.value_map().is_atomic_in_progress()
    }

    /// Aborts an atomic batch write operation.
    fn abort_atomic(&self) {
        self.program_id_map().abort_atomic();
        self.program_index_map().abort_atomic();
        self.mapping_id_map().abort_atomic();
        self.key_value_id_map().abort_atomic();
        self.key_map().abort_atomic();
        self.value_map().abort_atomic();
    }

    /// Finishes an atomic batch write operation.
    fn finish_atomic(&self) -> Result<()> {
        self.program_id_map().finish_atomic()?;
        self.program_index_map().finish_atomic()?;
        self.mapping_id_map().finish_atomic()?;
        self.key_value_id_map().finish_atomic()?;
        self.key_map().finish_atomic()?;
        self.value_map().finish_atomic()
    }

    /// Initializes the given `program ID` and `mapping name` in storage.
    fn initialize_mapping(&self, program_id: &ProgramID<N>, mapping_name: &Identifier<N>) -> Result<()> {
        // Ensure the mapping name does not already exist.
        if self.mapping_id_map().contains_key(&(*program_id, *mapping_name))? {
            bail!("Illegal operation: mapping '{mapping_name}' already exists in storage - cannot initialize again.")
        }

        // Compute the mapping ID.
        let mapping_id = N::hash_bhp1024(&(program_id, mapping_name).to_bits_le())?;
        // Ensure the mapping ID does not already exist.
        if self.key_value_id_map().contains_key(&mapping_id)? {
            bail!("Illegal operation: mapping ID '{mapping_id}' already exists in storage - cannot initialize again.")
        }

        // Retrieve the mapping names for the program ID.
        let mut mapping_names = match self.program_id_map().get_speculative(program_id)? {
            // If the program ID already exists, retrieve the mapping names.
            Some(mapping_names) => cow_to_cloned!(mapping_names),
            // If the program ID does not exist, initialize the mapping names.
            None => IndexSet::new(),
        };
        // Insert the new mapping name.
        mapping_names.insert(*mapping_name);

        // Retrieve the program index.
        let program_index = match self.program_index_map().get_speculative(program_id)? {
            Some(program_index) => cow_to_cloned!(program_index),
            None => match self.program_index_map().values().max() {
                Some(max_program_index) => max_program_index.saturating_add(1),
                None => 0,
            },
        };

        atomic_write_batch!(self, {
            // Update the program ID map with the new mapping name.
            self.program_id_map().insert(*program_id, mapping_names)?;
            // Update the program index map with the new program index.
            self.program_index_map().insert(*program_id, program_index)?;
            // Initialize the mapping ID map.
            self.mapping_id_map().insert((*program_id, *mapping_name), mapping_id)?;
            // Initialize the key-value ID map.
            self.key_value_id_map().insert(mapping_id, IndexMap::new())?;

            Ok(())
        });

        Ok(())
    }

    /// Stores the given `(key, value)` pair at the given `program ID` and `mapping name` in storage.
    /// If the `key` already exists, the method returns an error.
    fn insert_key_value(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        key: Plaintext<N>,
        value: Value<N>,
    ) -> Result<()> {
        // Retrieve the mapping ID.
        let mapping_id = match self.get_mapping_id(program_id, mapping_name)? {
            Some(mapping_id) => mapping_id,
            None => bail!("Illegal operation: mapping '{mapping_name}' is not initialized - cannot insert key-value."),
        };
        // Compute the key ID.
        let key_id = N::hash_bhp1024(&(mapping_id, N::hash_bhp1024(&key.to_bits_le())?).to_bits_le())?;
        // Compute the value ID.
        let value_id = N::hash_bhp1024(&(key_id, N::hash_bhp1024(&value.to_bits_le())?).to_bits_le())?;

        // Ensure the key ID does not already exist.
        if self.key_map().contains_key(&key_id)? {
            bail!("Illegal operation: key ID '{key_id}' already exists in storage - cannot insert again.")
        }
        // Retrieve the key-value IDs for the mapping ID.
        let mut key_value_ids = match self.key_value_id_map().get_speculative(&mapping_id)? {
            Some(key_value_ids) => cow_to_cloned!(key_value_ids),
            None => bail!("Illegal operation: mapping ID '{mapping_id}' is not initialized - cannot insert key-value."),
        };
        // Ensure the key ID does not already exist.
        if key_value_ids.contains_key(&key_id) {
            bail!("Illegal operation: key ID '{key_id}' already exists in storage - cannot insert key-value.");
        }
        // Insert the new key-value ID.
        key_value_ids.insert(key_id, value_id);

        atomic_write_batch!(self, {
            // Update the key-value ID map with the new key-value ID.
            self.key_value_id_map().insert(mapping_id, key_value_ids)?;
            // Insert the key.
            self.key_map().insert(key_id, key)?;
            // Insert the value.
            self.value_map().insert(key_id, value)?;

            Ok(())
        });

        Ok(())
    }

    /// Stores the given `(key, value)` pair at the given `program ID` and `mapping name` in storage.
    /// If the `key` does not exist, the `(key, value)` pair is initialized.
    /// If the `key` already exists, the `value` is overwritten.
    fn update_key_value(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        key: Plaintext<N>,
        value: Value<N>,
    ) -> Result<()> {
        // Retrieve the mapping ID.
        let mapping_id = match self.get_mapping_id(program_id, mapping_name)? {
            Some(mapping_id) => mapping_id,
            None => bail!("Illegal operation: mapping '{mapping_name}' is not initialized - cannot update key-value."),
        };
        // Compute the key ID.
        let key_id = N::hash_bhp1024(&(mapping_id, N::hash_bhp1024(&key.to_bits_le())?).to_bits_le())?;
        // Compute the value ID.
        let value_id = N::hash_bhp1024(&(key_id, N::hash_bhp1024(&value.to_bits_le())?).to_bits_le())?;

        // Retrieve the key-value IDs for the mapping ID.
        let mut key_value_ids = match self.key_value_id_map().get_speculative(&mapping_id)? {
            Some(key_value_ids) => cow_to_cloned!(key_value_ids),
            None => {
                bail!("Illegal operation: mapping ID '{mapping_id}' is not initialized - cannot update key-value.")
            }
        };
        // If the key ID does not exist, insert it in the key-value ID map.
        if self.key_map().get_speculative(&key_id)?.is_none() {
            // Ensure the key ID does not already exist.
            // If this fails, then there is inconsistent state, and likely data corruption.
            if key_value_ids.contains_key(&key_id) {
                bail!("Illegal operation: key ID '{key_id}' already exists in storage - cannot update key-value.");
            }
        }
        // Insert the new key-value ID.
        key_value_ids.insert(key_id, value_id);

        atomic_write_batch!(self, {
            // Update the key-value ID map with the new key-value ID.
            self.key_value_id_map().insert(mapping_id, key_value_ids)?;
            // Insert the key.
            self.key_map().insert(key_id, key)?;
            // Insert the value.
            self.value_map().insert(key_id, value)?;

            Ok(())
        });

        Ok(())
    }

    /// Removes the key-value pair for the given `program ID`, `mapping name`, and `key` from storage.
    fn remove_key_value(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        key: &Plaintext<N>,
    ) -> Result<()> {
        // Retrieve the mapping ID.
        let mapping_id = match self.get_mapping_id(program_id, mapping_name)? {
            Some(mapping_id) => mapping_id,
            None => bail!("Illegal operation: mapping '{mapping_name}' is not initialized - cannot remove key-value."),
        };
        // Compute the key ID.
        let key_id = N::hash_bhp1024(&(mapping_id, N::hash_bhp1024(&key.to_bits_le())?).to_bits_le())?;
        // Retrieve the key-value IDs for the mapping ID.
        let mut key_value_ids = match self.key_value_id_map().get_speculative(&mapping_id)? {
            Some(key_value_ids) => cow_to_cloned!(key_value_ids),
            None => bail!("Illegal operation: mapping ID '{mapping_id}' is not initialized - cannot remove key-value."),
        };
        // Ensure the key ID exists.
        if !key_value_ids.contains_key(&key_id) {
            bail!("Illegal operation: key ID '{key_id}' does not exist in storage - cannot remove key-value.");
        }
        // Remove the key ID.
        key_value_ids.remove(&key_id);

        atomic_write_batch!(self, {
            // Update the key-value ID map with the new key ID.
            self.key_value_id_map().insert(mapping_id, key_value_ids)?;
            // Remove the key.
            self.key_map().remove(&key_id)?;
            // Remove the value.
            self.value_map().remove(&key_id)?;

            Ok(())
        });

        Ok(())
    }

    /// Removes the mapping for the given `program ID` and `mapping name` from storage,
    /// along with all associated key-value pairs in storage.
    fn remove_mapping(&self, program_id: &ProgramID<N>, mapping_name: &Identifier<N>) -> Result<()> {
        // Retrieve the mapping ID.
        let mapping_id = match self.get_mapping_id(program_id, mapping_name)? {
            Some(mapping_id) => mapping_id,
            None => bail!("Illegal operation: mapping '{mapping_name}' is not initialized - cannot remove mapping."),
        };
        // Retrieve the key-value IDs for the mapping ID.
        let key_value_ids = match self.key_value_id_map().get_speculative(&mapping_id)? {
            Some(key_value_ids) => key_value_ids,
            None => bail!("Illegal operation: mapping ID '{mapping_id}' is not initialized - cannot remove mapping."),
        };

        // Retrieve the mapping names.
        let mut mapping_names = match self.program_id_map().get_speculative(program_id)? {
            Some(mapping_names) => cow_to_cloned!(mapping_names),
            None => bail!("Illegal operation: program ID '{program_id}' is not initialized - cannot remove mapping."),
        };
        // Ensure the mapping name exists.
        if !mapping_names.contains(mapping_name) {
            bail!("Illegal operation: mapping '{mapping_name}' does not exist in storage - cannot remove mapping.");
        }
        // Remove the mapping name.
        mapping_names.remove(mapping_name);

        atomic_write_batch!(self, {
            // Update the mapping names.
            self.program_id_map().insert(*program_id, mapping_names)?;
            // Remove the mapping ID.
            self.mapping_id_map().remove(&(*program_id, *mapping_name))?;
            // Remove the key IDs.
            self.key_value_id_map().remove(&mapping_id)?;
            // Remove the keys.
            for key_id in key_value_ids.keys() {
                self.key_map().remove(key_id)?;
                self.value_map().remove(key_id)?;
            }

            Ok(())
        });

        Ok(())
    }

    /// Removes the program for the given `program ID` from storage,
    /// along with all associated mappings and key-value pairs in storage.
    fn remove_program(&self, program_id: &ProgramID<N>) -> Result<()> {
        // Retrieve the mapping names.
        let mapping_names = match self.program_id_map().get_speculative(program_id)? {
            Some(mapping_names) => mapping_names,
            None => bail!("Illegal operation: program ID '{program_id}' is not initialized - cannot remove mapping."),
        };

        // Retrieve the deployment index.
        let deployment_index = match self.program_index_map().get_speculative(program_id)? {
            Some(deployment_index) => deployment_index,
            None => bail!("Illegal operation: program ID '{program_id}' is not initialized - cannot remove index."),
        };

        atomic_write_batch!(self, {
            // Update the mapping names.
            self.program_id_map().remove(program_id)?;

            // Update the deployment index.
            self.program_index_map().remove(program_id)?;

            // Update each subsequent deployment index.
            for (program_id, index) in self.program_index_map().iter() {
                if *index > *deployment_index {
                    self.program_index_map().insert(*program_id, index.saturating_sub(1))?;
                }
            }

            // Remove each mapping.
            for mapping_name in mapping_names.iter() {
                // Retrieve the mapping ID.
                let mapping_id = match self.get_mapping_id(program_id, mapping_name)? {
                    Some(mapping_id) => mapping_id,
                    None => {
                        bail!("Illegal operation: mapping '{mapping_name}' is not initialized - cannot remove mapping.")
                    }
                };
                // Retrieve the key-value IDs for the mapping ID.
                let key_value_ids = match self.key_value_id_map().get_speculative(&mapping_id)? {
                    Some(key_value_ids) => key_value_ids,
                    None => {
                        bail!(
                            "Illegal operation: mapping ID '{mapping_id}' is not initialized - cannot remove mapping."
                        )
                    }
                };

                // Remove the mapping ID.
                self.mapping_id_map().remove(&(*program_id, *mapping_name))?;
                // Remove the key IDs.
                self.key_value_id_map().remove(&mapping_id)?;
                // Remove the keys.
                for key_id in key_value_ids.keys() {
                    self.key_map().remove(key_id)?;
                    self.value_map().remove(key_id)?;
                }
            }

            Ok(())
        });

        Ok(())
    }

    /// Returns `true` if the given `program ID` exist.
    fn contains_program(&self, program_id: &ProgramID<N>) -> Result<bool> {
        self.program_id_map().contains_key(program_id).and(self.program_index_map().contains_key(program_id))
    }

    /// Returns `true` if the given `program ID` and `mapping name` exist.
    fn contains_mapping(&self, program_id: &ProgramID<N>, mapping_name: &Identifier<N>) -> Result<bool> {
        self.mapping_id_map().contains_key(&(*program_id, *mapping_name))
    }

    /// Returns `true` if the given `program ID`, `mapping name`, and `key` exist.
    fn contains_key(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        key: &Plaintext<N>,
    ) -> Result<bool> {
        // Retrieve the mapping ID.
        let mapping_id = match self.get_mapping_id(program_id, mapping_name)? {
            Some(mapping_id) => mapping_id,
            None => return Ok(false),
        };
        // Compute the key ID.
        let key_id = N::hash_bhp1024(&(mapping_id, N::hash_bhp1024(&key.to_bits_le())?).to_bits_le())?;
        // Return whether the key ID exists.
        self.key_map().contains_key(&key_id)
    }

    /// Returns the mapping names for the given `program ID`.
    fn get_mapping_names(&self, program_id: &ProgramID<N>) -> Result<Option<IndexSet<Identifier<N>>>> {
        // Retrieve the mapping names.
        match self.program_id_map().get_speculative(program_id)? {
            Some(names) => Ok(Some(cow_to_cloned!(names))),
            None => Ok(None),
        }
    }

    /// Returns the mapping ID for the given `program ID` and `mapping name`.
    fn get_mapping_id(&self, program_id: &ProgramID<N>, mapping_name: &Identifier<N>) -> Result<Option<Field<N>>> {
        match self.mapping_id_map().get_speculative(&(*program_id, *mapping_name))? {
            Some(mapping_id) => Ok(Some(cow_to_copied!(mapping_id))),
            None => Ok(None),
        }
    }

    /// Returns the key ID for the given `program ID`, `mapping name`, and `key`.
    fn get_key_id(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        key: &Plaintext<N>,
    ) -> Result<Option<Field<N>>> {
        // Retrieve the mapping ID.
        let mapping_id = match self.get_mapping_id(program_id, mapping_name)? {
            Some(mapping_id) => mapping_id,
            None => return Ok(None),
        };
        // Compute the key ID.
        let key_id = N::hash_bhp1024(&(mapping_id, N::hash_bhp1024(&key.to_bits_le())?).to_bits_le())?;
        // Ensure the key ID exists.
        match self.key_map().contains_key(&key_id)? {
            true => Ok(Some(key_id)),
            false => Ok(None),
        }
    }

    /// Returns the key for the given `key ID`.
    fn get_key(&self, key_id: &Field<N>) -> Result<Option<Plaintext<N>>> {
        match self.key_map().get_speculative(key_id)? {
            Some(key) => Ok(Some(cow_to_cloned!(key))),
            None => Ok(None),
        }
    }

    /// Returns the value for the given `program ID`, `mapping name`, and `key`.
    fn get_value(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        key: &Plaintext<N>,
    ) -> Result<Option<Value<N>>> {
        // Retrieve the key ID.
        match self.get_key_id(program_id, mapping_name, key)? {
            // Retrieve the value.
            Some(key_id) => self.get_value_from_key_id(&key_id),
            None => {
                // TODO (raychu86): Confirm this is the correct behavior in accordance to #1251.
                // Construct the `mapping ID`.
                let mapping_id = N::hash_bhp1024(&(program_id, mapping_name).to_bits_le())?;
                // Construct the `key ID`.
                let key_id = N::hash_bhp1024(&(mapping_id, N::hash_bhp1024(&key.to_bits_le())?).to_bits_le())?;

                // Check if the key ID exists.
                match self.key_map().get_speculative(&key_id)? {
                    Some(_) => self.get_value_from_key_id(&key_id),
                    None => Ok(None),
                }
            }
        }
    }

    /// Returns the value for the given `key ID`.
    fn get_value_from_key_id(&self, key_id: &Field<N>) -> Result<Option<Value<N>>> {
        match self.value_map().get_speculative(key_id)? {
            Some(value) => Ok(Some(cow_to_cloned!(value))),
            None => Ok(None),
        }
    }

    /// Returns the checksum.
    fn get_checksum(&self) -> Result<Field<N>> {
        // Compute all mapping checksums.
        let preimage: BTreeMap<_, _> = self
            .key_value_id_map()
            .iter()
            .map(|(mapping_id, key_value_ids)| {
                // Convert the mapping ID and all value IDs to concatenated bits.
                let preimage = mapping_id
                    .to_bits_le()
                    .into_iter()
                    .chain(key_value_ids.values().flat_map(|value_id| value_id.to_bits_le()));
                // Compute the mapping checksum as `Hash( mapping_id || all value IDs )`.
                let mapping_checksum = N::hash_bhp1024(&preimage.collect::<Vec<_>>())?;
                // Return the mapping ID and mapping checksum.
                Ok::<_, Error>((mapping_id, mapping_checksum.to_bits_le()))
            })
            .try_collect()?;
        // Compute the checksum as `Hash( all mapping checksums )`.
        N::hash_bhp1024(&preimage.into_values().flatten().collect::<Vec<_>>())
    }

    // TODO (raychu86): This depends on the `Map`s being deterministically ordered (by insertion).
    /// Returns the Merkle tree of program state.
    fn to_finalize_tree(&self) -> Result<FinalizeTree<N>> {
        // Initialize a list of program trees.
        let mut program_trees: IndexMap<u32, ProgramTree<N>> = IndexMap::new();

        // TODO (raychu86): Parallelize this.
        // Iterate through all the programs and construct the program trees.
        for (program_id, index) in self.program_index_map().iter() {
            // Construct the program tree.
            let program_tree = self.to_program_tree(&program_id, None)?;

            // Insert the program tree to the list of program trees.
            program_trees.insert(*index, program_tree);
        }

        // Sort the program trees by index.
        program_trees.sort_keys();

        // Construct the storage tree.
        N::merkle_tree_bhp(&cfg_iter!(program_trees).map(|(_, tree)| tree.root().to_bits_le()).collect::<Vec<_>>())
    }

    /// Returns the Merkle tree of the given program's mapping state.
    fn to_program_tree(
        &self,
        program_id: &ProgramID<N>,
        optional_updates: Option<&[MerkleTreeUpdate<N>]>,
    ) -> Result<ProgramTree<N>> {
        // Retrieve the mapping names for the given program ID.
        let mapping_names = &*self.program_id_map().get_speculative(program_id)?.unwrap_or_default();

        // Construct a mapping trees.
        let mut mapping_trees = cfg_iter!(mapping_names)
            .map(|mapping_name| self.to_mapping_tree(program_id, mapping_name, optional_updates))
            .collect::<Result<IndexMap<_, _>>>()?;

        // Check if any mappings need to be removed.
        if let Some(updates) = optional_updates {
            // Iterate through all the mapping updates.
            for update in updates {
                match update {
                    MerkleTreeUpdate::InsertMapping(mapping_id) => {
                        // Insert a new mapping tree.
                        mapping_trees.insert(*mapping_id, N::merkle_tree_bhp(&[])?);
                    }
                    MerkleTreeUpdate::RemoveMapping(mapping_id) => {
                        // Remove the mapping tree.
                        mapping_trees.shift_remove_entry(mapping_id);
                    }
                    _ => {}
                }
            }
        }

        // Construct the program tree with the mapping_trees.
        let mapping_roots = cfg_iter!(mapping_trees).map(|(_, tree)| tree.root().to_bits_le()).collect::<Vec<_>>();

        // Construct the program tree.
        N::merkle_tree_bhp(&mapping_roots)
    }

    /// Returns the `mapping_id` and the merkle tree of a program's mapping state.
    fn to_mapping_tree(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        optional_updates: Option<&[MerkleTreeUpdate<N>]>,
    ) -> Result<(Field<N>, MappingTree<N>)> {
        // Get the mapping ID.
        let mapping_id = self
            .get_mapping_id(program_id, mapping_name)?
            .ok_or_else(|| anyhow!("Missing mapping ID for {program_id}/{mapping_name}"))?;

        // Get the key_values for the mapping id.
        let key_values = self
            .key_value_id_map()
            .get_speculative(&mapping_id)?
            .ok_or_else(|| anyhow!("Missing key values for mapping id {mapping_id}"))?;

        // Construct the leaves for the mapping tree.
        let mut key_value_leaves = cfg_iter!(key_values).map(|(_, value_id)| value_id.to_bits_le()).collect::<Vec<_>>();

        // Perform the merkle tree updates if they exist.
        if let Some(optional_updates) = optional_updates {
            for update in optional_updates {
                // Skip the update if it isn't relevant to this mapping.
                if update.mapping_id() != mapping_id {
                    continue;
                }

                // Perform the update.
                match update {
                    MerkleTreeUpdate::InsertValue(_, _, leaf) => {
                        // Insert the new leaf.
                        key_value_leaves.push(leaf.to_bits_le());
                    }
                    MerkleTreeUpdate::UpdateValue(_, index, _, leaf) => {
                        let elem = key_value_leaves
                            .get_mut(*index)
                            .ok_or_else(|| anyhow!("Missing key value leaf at index {index}"))?;
                        *elem = leaf.to_bits_le();
                    }
                    MerkleTreeUpdate::RemoveValue(_, index) => {
                        // Remove the leaf.
                        key_value_leaves.remove(*index);
                    }
                    _ => continue,
                }
            }
        }

        // Construct the mapping tree.
        let mapping_tree = N::merkle_tree_bhp(&key_value_leaves)?;

        Ok((mapping_id, mapping_tree))
    }
}

/// An in-memory program state storage.
#[derive(Clone)]
pub struct FinalizeMemory<N: Network> {
    /// The program ID map.
    program_id_map: MemoryMap<ProgramID<N>, IndexSet<Identifier<N>>>,
    /// The program index map.
    program_index_map: MemoryMap<ProgramID<N>, u32>,
    /// The mapping ID map.
    mapping_id_map: MemoryMap<(ProgramID<N>, Identifier<N>), Field<N>>,
    /// The key-value ID map.
    key_value_id_map: MemoryMap<Field<N>, IndexMap<Field<N>, Field<N>>>,
    /// The key map.
    key_map: MemoryMap<Field<N>, Plaintext<N>>,
    /// The value map.
    value_map: MemoryMap<Field<N>, Value<N>>,
    /// The optional development ID.
    dev: Option<u16>,
}

#[rustfmt::skip]
impl<N: Network> FinalizeStorage<N> for FinalizeMemory<N> {
    type ProgramIDMap = MemoryMap<ProgramID<N>, IndexSet<Identifier<N>>>;
    type ProgramIndexMap = MemoryMap<ProgramID<N>, u32>;
    type MappingIDMap = MemoryMap<(ProgramID<N>, Identifier<N>), Field<N>>;
    type KeyValueIDMap = MemoryMap<Field<N>, IndexMap<Field<N>, Field<N>>>;
    type KeyMap = MemoryMap<Field<N>, Plaintext<N>>;
    type ValueMap = MemoryMap<Field<N>, Value<N>>;

    /// Initializes the program state storage.
    fn open(dev: Option<u16>) -> Result<Self> {
        Ok(Self {
            program_id_map: MemoryMap::default(),
            program_index_map: MemoryMap::default(),
            mapping_id_map: MemoryMap::default(),
            key_value_id_map: MemoryMap::default(),
            key_map: MemoryMap::default(),
            value_map: MemoryMap::default(),
            dev,
        })
    }

    /// Returns the program ID map.
    fn program_id_map(&self) -> &Self::ProgramIDMap {
        &self.program_id_map
    }

    /// Returns the program index map.
    fn program_index_map(&self) -> &Self::ProgramIndexMap {
        &self.program_index_map
    }

    /// Returns the mapping ID map.
    fn mapping_id_map(&self) -> &Self::MappingIDMap {
        &self.mapping_id_map
    }

    /// Returns the key-value ID map.
    fn key_value_id_map(&self) -> &Self::KeyValueIDMap {
        &self.key_value_id_map
    }

    /// Returns the key map.
    fn key_map(&self) -> &Self::KeyMap {
        &self.key_map
    }

    /// Returns the value map.
    fn value_map(&self) -> &Self::ValueMap {
        &self.value_map
    }

    /// Returns the optional development ID.
    fn dev(&self) -> Option<u16> {
        self.dev
    }
}

/// The finalize store.
#[derive(Clone)]
pub struct FinalizeStore<N: Network, P: FinalizeStorage<N>> {
    /// The finalize storage.
    storage: P,
    /// The finalize tree.
    pub(crate) tree: Arc<RwLock<FinalizeTree<N>>>,

    /// The speculate lock. This is used to prevent individual merkle tree operations in favor of
    ///  a batched update via `Speculate`.
    pub(crate) is_speculate: Arc<AtomicBool>,

    /// PhantomData.
    _phantom: PhantomData<N>,
}

impl<N: Network, P: FinalizeStorage<N>> FinalizeStore<N, P> {
    /// Initializes the finalize store.
    pub fn open(dev: Option<u16>) -> Result<Self> {
        // Initialize the finalize storage.
        let storage = P::open(dev)?;

        // Compute the finalize tree.
        let tree = Arc::new(RwLock::new(storage.to_finalize_tree()?));

        Ok(Self { storage, tree, is_speculate: Default::default(), _phantom: PhantomData })
    }

    /// Initializes a finalize store from storage.
    pub fn from(storage: P) -> Result<Self> {
        // Compute the finalize tree.
        let tree = Arc::new(RwLock::new(storage.to_finalize_tree()?));

        Ok(Self { storage, tree, is_speculate: Default::default(), _phantom: PhantomData })
    }

    /// Initializes the given `program ID` and `mapping name` in storage.
    pub fn initialize_mapping(&self, program_id: &ProgramID<N>, mapping_name: &Identifier<N>) -> Result<()> {
        // If we are in speculate mode, then we do not need to update the storage tree.
        if self.is_speculate.load(Ordering::SeqCst) {
            // Initialize the mapping
            self.storage.initialize_mapping(program_id, mapping_name)?;
        } else {
            // Acquire the write lock on the storage tree.
            let mut tree = self.tree.write();

            // Construct the updated storage tree.
            let updated_tree = {
                // Compute the mapping ID.
                let mapping_id = N::hash_bhp1024(&(program_id, mapping_name).to_bits_le())?;

                // Construct the updated program tree.
                let program_tree =
                    self.storage.to_program_tree(program_id, Some(&[MerkleTreeUpdate::InsertMapping(mapping_id)]))?;

                match self.storage.program_index_map().get(program_id)? {
                    Some(program_id_index) => {
                        // Construct the updated storage tree.
                        tree.prepare_update(usize::try_from(*program_id_index)?, &program_tree.root().to_bits_le())?
                    }
                    None => {
                        // Add the program tree root to the tree if the program ID does not exist yet.
                        tree.prepare_append(&[program_tree.root().to_bits_le()])?
                    }
                }
            };

            // Initialize the mapping
            self.storage.initialize_mapping(program_id, mapping_name)?;

            // Update the storage tree.
            *tree = updated_tree;
        }

        Ok(())
    }

    /// Stores the given `(key, value)` pair at the given `program ID` and `mapping name` in storage.
    /// If the `key` already exists, the method returns an error.
    pub fn insert_key_value(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        key: Plaintext<N>,
        value: Value<N>,
    ) -> Result<()> {
        // If we are in speculate mode, then we do not need to update the storage tree.
        if self.is_speculate.load(Ordering::SeqCst) {
            // Insert the key-value.
            self.storage.insert_key_value(program_id, mapping_name, key, value)?;
        } else {
            // Acquire the write lock on the storage tree.
            let mut tree = self.tree.write();

            // Construct the updated storage tree.
            let updated_tree = {
                // Retrieve the mapping ID.
                let mapping_id = match self.storage.get_mapping_id(program_id, mapping_name)? {
                    Some(mapping_id) => mapping_id,
                    None => {
                        bail!(
                            "Illegal operation: mapping '{mapping_name}' is not initialized - cannot insert key-value."
                        )
                    }
                };

                // Compute the key ID.
                let key_id = N::hash_bhp1024(&(mapping_id, N::hash_bhp1024(&key.to_bits_le())?).to_bits_le())?;
                // Compute the value ID.
                let value_id = N::hash_bhp1024(&(key_id, N::hash_bhp1024(&value.to_bits_le())?).to_bits_le())?;

                // Construct the updated program tree.
                let program_tree = self.storage.to_program_tree(
                    program_id,
                    Some(&[MerkleTreeUpdate::InsertValue(mapping_id, key_id, value_id)]),
                )?;

                // Fetch the index of the program ID.
                let program_id_index = match self.storage.program_index_map().get(program_id)? {
                    Some(program_id_index) => *program_id_index,
                    None => bail!("Missing program ID '{program_id}' in program index map"),
                };

                // Construct the updated storage tree.
                tree.prepare_update(usize::try_from(program_id_index)?, &program_tree.root().to_bits_le())?
            };

            // Insert the key-value pair.
            self.storage.insert_key_value(program_id, mapping_name, key, value)?;

            // Update the storage tree.
            *tree = updated_tree;
        }

        Ok(())
    }

    /// Stores the given `(key, value)` pair at the given `program ID` and `mapping name` in storage.
    /// If the `key` does not exist, the `(key, value)` pair is initialized.
    /// If the `key` already exists, the `value` is overwritten.
    pub fn update_key_value(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        key: Plaintext<N>,
        value: Value<N>,
    ) -> Result<()> {
        // If we are in speculate mode, then we do not need to update the storage tree.
        if self.is_speculate.load(Ordering::SeqCst) {
            // Update the key-value pair.
            self.storage.update_key_value(program_id, mapping_name, key, value)?;
        } else {
            // Acquire the write lock on the storage tree.
            let mut tree = self.tree.write();

            // Construct the updated storage tree.
            let updated_tree = {
                // Retrieve the mapping ID.
                let mapping_id = match self.storage.get_mapping_id(program_id, mapping_name)? {
                    Some(mapping_id) => mapping_id,
                    None => {
                        bail!(
                            "Illegal operation: mapping '{mapping_name}' is not initialized - cannot insert key-value."
                        )
                    }
                };

                // Compute the key ID.
                let key_id = N::hash_bhp1024(&(mapping_id, N::hash_bhp1024(&key.to_bits_le())?).to_bits_le())?;
                // Compute the value ID.
                let value_id = N::hash_bhp1024(&(key_id, N::hash_bhp1024(&value.to_bits_le())?).to_bits_le())?;

                // Fetch the index of the key ID.
                let key_value_map = self
                    .storage
                    .key_value_id_map()
                    .get(&mapping_id)?
                    .ok_or_else(|| anyhow!("Missing mapping ID {mapping_id}"))?;

                // Construct the update operation. If the key ID does not exist, insert it.
                let update = match key_value_map.get_index_of(&key_id) {
                    Some(key_id_index) => MerkleTreeUpdate::UpdateValue(mapping_id, key_id_index, key_id, value_id),
                    None => MerkleTreeUpdate::InsertValue(mapping_id, key_id, value_id),
                };

                // Construct the updated program tree.
                let program_tree = self.storage.to_program_tree(program_id, Some(&[update]))?;

                // Fetch the index of the program ID.
                let program_id_index = match self.storage.program_index_map().get(program_id)? {
                    Some(program_id_index) => *program_id_index,
                    None => bail!("Missing program ID '{program_id}' in program index map"),
                };

                // Construct the updated storage tree.
                tree.prepare_update(usize::try_from(program_id_index)?, &program_tree.root().to_bits_le())?
            };

            // Update the key-value pair.
            self.storage.update_key_value(program_id, mapping_name, key, value)?;

            // Update the storage tree.
            *tree = updated_tree;
        }

        Ok(())
    }

    /// Removes the key-value pair for the given `program ID`, `mapping name`, and `key` from storage.
    pub fn remove_key_value(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        key: &Plaintext<N>,
    ) -> Result<()> {
        // If we are in speculate mode, then we do not need to update the storage tree.
        if self.is_speculate.load(Ordering::SeqCst) {
            // Remove the key-value pair.
            self.storage.remove_key_value(program_id, mapping_name, key)?;
        } else {
            // Acquire the write lock on the storage tree.
            let mut tree = self.tree.write();

            // Construct the updated storage tree.
            let updated_tree = {
                // Retrieve the mapping ID.
                let mapping_id = match self.storage.get_mapping_id(program_id, mapping_name)? {
                    Some(mapping_id) => mapping_id,
                    None => {
                        bail!(
                            "Illegal operation: mapping '{mapping_name}' is not initialized - cannot insert key-value."
                        )
                    }
                };

                // Compute the key ID.
                let key_id = N::hash_bhp1024(&(mapping_id, N::hash_bhp1024(&key.to_bits_le())?).to_bits_le())?;

                // Fetch the index of the key ID.
                let key_value_map = self
                    .storage
                    .key_value_id_map()
                    .get(&mapping_id)?
                    .ok_or_else(|| anyhow!("Missing mapping ID {mapping_id}"))?;
                let key_id_index = key_value_map
                    .get_index_of(&key_id)
                    .ok_or_else(|| anyhow!("Missing key ID '{key_id}' in key id map"))?;

                // Construct the updated program tree.
                let program_tree = self
                    .storage
                    .to_program_tree(program_id, Some(&[MerkleTreeUpdate::RemoveValue(mapping_id, key_id_index)]))?;

                // Fetch the index of the program ID.
                let program_id_index = match self.storage.program_index_map().get(program_id)? {
                    Some(program_id_index) => *program_id_index,
                    None => bail!("Missing program ID '{program_id}' in program index map"),
                };

                // Construct the updated storage tree.
                tree.prepare_update(usize::try_from(program_id_index)?, &program_tree.root().to_bits_le())?
            };

            // Remove the key-value pair.
            self.storage.remove_key_value(program_id, mapping_name, key)?;

            // Update the storage tree.
            *tree = updated_tree;
        }

        Ok(())
    }

    /// Removes the mapping for the given `program ID` and `mapping name` from storage,
    /// along with all associated key-value pairs in storage.
    pub fn remove_mapping(&self, program_id: &ProgramID<N>, mapping_name: &Identifier<N>) -> Result<()> {
        // If we are in speculate mode, then we do not need to update the storage tree.
        if self.is_speculate.load(Ordering::SeqCst) {
            // Remove the mapping.
            self.storage.remove_mapping(program_id, mapping_name)?;
        } else {
            // Acquire the write lock on the storage tree.
            let mut tree = self.tree.write();

            // Construct the updated storage tree.
            let updated_tree = {
                // Retrieve the mapping ID.
                let mapping_id = match self.storage.get_mapping_id(program_id, mapping_name)? {
                    Some(mapping_id) => mapping_id,
                    None => {
                        bail!(
                            "Illegal operation: mapping '{mapping_name}' is not initialized - cannot insert key-value."
                        )
                    }
                };

                // Construct the updated program tree.
                let program_tree =
                    self.storage.to_program_tree(program_id, Some(&[MerkleTreeUpdate::RemoveMapping(mapping_id)]))?;

                // Fetch the index of the program ID.
                let program_id_index = match self.storage.program_index_map().get(program_id)? {
                    Some(program_id_index) => *program_id_index,
                    None => bail!("Missing program ID '{program_id}' in program index map"),
                };

                // Construct the updated storage tree.
                tree.prepare_update(usize::try_from(program_id_index)?, &program_tree.root().to_bits_le())?
            };

            // Remove the mapping.
            self.storage.remove_mapping(program_id, mapping_name)?;

            // Update the storage tree.
            *tree = updated_tree;
        }

        Ok(())
    }

    /// Removes the program for the given `program ID` from storage,
    /// along with all associated mappings and key-value pairs in storage.
    pub fn remove_program(&self, program_id: &ProgramID<N>) -> Result<()> {
        // If we are in speculate mode, then we do not need to update the storage tree.
        if self.is_speculate.load(Ordering::SeqCst) {
            // Remove the program..
            self.storage.remove_program(program_id)?;
        } else {
            // Acquire the write lock on the storage tree.
            let mut tree = self.tree.write();

            // Remove the program..
            self.storage.remove_program(program_id)?;

            // TODO (raychu86): Have a "shift_update" method that shifts the leaves.
            // Construct the updated storage tree.
            let updated_tree = self.storage.to_finalize_tree()?;

            // TODO (raychu86) Make sure the operations are atomic.
            *tree = updated_tree;
        }

        Ok(())
    }

    /// Starts an atomic batch write operation.
    pub fn start_atomic(&self) {
        self.storage.start_atomic();
    }

    /// Checks if an atomic batch is in progress.
    pub fn is_atomic_in_progress(&self) -> bool {
        self.storage.is_atomic_in_progress()
    }

    /// Aborts an atomic batch write operation.
    pub fn abort_atomic(&self) {
        self.storage.abort_atomic();
    }

    /// Finishes an atomic batch write operation.
    pub fn finish_atomic(&self) -> Result<()> {
        self.storage.finish_atomic()
    }

    /// Returns the optional development ID.
    pub fn dev(&self) -> Option<u16> {
        self.storage.dev()
    }
}

impl<N: Network, P: FinalizeStorage<N>> FinalizeStore<N, P> {
    /// Returns `true` if the given `program ID` exist.
    pub fn contains_program(&self, program_id: &ProgramID<N>) -> Result<bool> {
        self.storage.contains_program(program_id)
    }

    /// Returns `true` if the given `program ID` and `mapping name` exist.
    pub fn contains_mapping(&self, program_id: &ProgramID<N>, mapping_name: &Identifier<N>) -> Result<bool> {
        self.storage.contains_mapping(program_id, mapping_name)
    }

    /// Returns `true` if the given `program ID`, `mapping name`, and `key` exist.
    pub fn contains_key(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        key: &Plaintext<N>,
    ) -> Result<bool> {
        self.storage.contains_key(program_id, mapping_name, key)
    }
}

impl<N: Network, P: FinalizeStorage<N>> FinalizeStore<N, P> {
    /// Returns the current storage root.
    pub fn current_storage_root(&self) -> Field<N> {
        *self.tree.read().root()
    }

    /// Returns the mapping names for the given `program ID`.
    pub fn get_mapping_names(&self, program_id: &ProgramID<N>) -> Result<Option<IndexSet<Identifier<N>>>> {
        self.storage.get_mapping_names(program_id)
    }

    /// Returns the index for the given `program ID`, `mapping name`, and `key` if it exists.
    pub fn get_key_index(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        key: &Plaintext<N>,
    ) -> Result<Option<u32>> {
        match self.storage.get_mapping_id(program_id, mapping_name)? {
            Some(mapping_id) => match self.storage.key_value_id_map().get(&mapping_id)? {
                Some(key_value_map) => {
                    // Compute the key ID.
                    let key_id = N::hash_bhp1024(&(mapping_id, N::hash_bhp1024(&key.to_bits_le())?).to_bits_le())?;

                    Ok(key_value_map.get_index_of(&key_id).map(|index| index as u32))
                }
                None => Ok(None),
            },
            None => Ok(None),
        }
    }

    /// Returns the value for the given `program ID`, `mapping name`, and `key`.
    pub fn get_value(
        &self,
        program_id: &ProgramID<N>,
        mapping_name: &Identifier<N>,
        key: &Plaintext<N>,
    ) -> Result<Option<Value<N>>> {
        self.storage.get_value(program_id, mapping_name, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use console::network::Testnet3;

    type CurrentNetwork = Testnet3;

    /// Checks `initialize_mapping`, `insert_key_value`, `remove_key_value`, and `remove_mapping`.
    fn check_initialize_insert_remove<N: Network>(
        finalize_store: &FinalizeStore<N, FinalizeMemory<N>>,
        program_id: ProgramID<N>,
        mapping_name: Identifier<N>,
    ) {
        // Prepare a key and value.
        let key = Plaintext::from_str("123456789field").unwrap();
        let value = Value::from_str("987654321u128").unwrap();

        // Ensure the program ID does not exist.
        assert!(!finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name does not exist.
        assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure removing an un-initialized mapping fails.
        assert!(finalize_store.remove_mapping(&program_id, &mapping_name).is_err());

        // Now, initialize the mapping.
        finalize_store.initialize_mapping(&program_id, &mapping_name).unwrap();
        // Ensure the program ID got initialized.
        assert!(finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name got initialized.
        assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure the key did not get initialized.
        assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
        // Ensure the value returns None.
        assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Insert a (key, value) pair.
        finalize_store.insert_key_value(&program_id, &mapping_name, key.clone(), value.clone()).unwrap();
        // Ensure the program ID is still initialized.
        assert!(finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name is still initialized.
        assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure the key got initialized.
        assert!(finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
        // Ensure the value returns Some(value).
        assert_eq!(value, finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().unwrap());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Ensure removing the key succeeds.
        finalize_store.remove_key_value(&program_id, &mapping_name, &key).unwrap();
        // Ensure the program ID is still initialized.
        assert!(finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name is still initialized.
        assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure the key got removed.
        assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
        // Ensure the value returns None.
        assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Ensure removing the mapping succeeds.
        finalize_store.remove_mapping(&program_id, &mapping_name).unwrap();
        // Ensure the program ID is still initialized.
        assert!(finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name is no longer initialized.
        assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure the key is still removed.
        assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
        // Ensure the value still returns None.
        assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Ensure removing the program succeeds.
        finalize_store.remove_program(&program_id).unwrap();
        // Ensure the program ID is no longer initialized.
        assert!(!finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name is still no longer initialized.
        assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure the key is still removed.
        assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
        // Ensure the value still returns None.
        assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());
    }

    /// Checks `initialize_mapping`, `update_key_value`, `remove_key_value`, and `remove_mapping`.
    fn check_initialize_update_remove<N: Network>(
        finalize_store: &FinalizeStore<N, FinalizeMemory<N>>,
        program_id: ProgramID<N>,
        mapping_name: Identifier<N>,
    ) {
        // Prepare a key and value.
        let key = Plaintext::from_str("123456789field").unwrap();
        let value = Value::from_str("987654321u128").unwrap();

        // Ensure the program ID does not exist.
        assert!(!finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name does not exist.
        assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure removing an un-initialized mapping fails.
        assert!(finalize_store.remove_mapping(&program_id, &mapping_name).is_err());

        // Now, initialize the mapping.
        finalize_store.initialize_mapping(&program_id, &mapping_name).unwrap();
        // Ensure the program ID got initialized.
        assert!(finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name got initialized.
        assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure the key did not get initialized.
        assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
        // Ensure the value returns None.
        assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Update a (key, value) pair.
        finalize_store.update_key_value(&program_id, &mapping_name, key.clone(), value.clone()).unwrap();
        // Ensure the program ID is still initialized.
        assert!(finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name is still initialized.
        assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure the key got initialized.
        assert!(finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
        // Ensure the value returns Some(value).
        assert_eq!(value, finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().unwrap());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Ensure calling `insert_key_value` with the same key and value fails.
        assert!(finalize_store.insert_key_value(&program_id, &mapping_name, key.clone(), value.clone()).is_err());
        // Ensure the key is still initialized.
        assert!(finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
        // Ensure the value still returns Some(value).
        assert_eq!(value, finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().unwrap());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Ensure calling `update_key_value` with the same key and value succeeds.
        finalize_store.update_key_value(&program_id, &mapping_name, key.clone(), value.clone()).unwrap();
        // Ensure the key is still initialized.
        assert!(finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
        // Ensure the value still returns Some(value).
        assert_eq!(value, finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().unwrap());

        {
            // Prepare the same key and different value.
            let new_value = Value::from_str("123456789u128").unwrap();

            // Ensure calling `insert_key_value` with a different key and value fails.
            assert!(
                finalize_store.insert_key_value(&program_id, &mapping_name, key.clone(), new_value.clone()).is_err()
            );
            // Ensure the key is still initialized.
            assert!(finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value still returns Some(value).
            assert_eq!(value, finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().unwrap());

            // Ensure calling `update_key_value` with a different key and value succeeds.
            finalize_store.update_key_value(&program_id, &mapping_name, key.clone(), new_value.clone()).unwrap();
            // Ensure the key is still initialized.
            assert!(finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns Some(new_value).
            assert_eq!(new_value, finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().unwrap());
            // Ensure that the storage tree is updated correctly.
            assert_eq!(
                finalize_store.current_storage_root(),
                *finalize_store.storage.to_finalize_tree().unwrap().root()
            );

            // Ensure calling `update_key_value` with the same key and original value succeeds.
            finalize_store.update_key_value(&program_id, &mapping_name, key.clone(), value.clone()).unwrap();
            // Ensure the key is still initialized.
            assert!(finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns Some(value).
            assert_eq!(value, finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().unwrap());
            // Ensure that the storage tree is updated correctly.
            assert_eq!(
                finalize_store.current_storage_root(),
                *finalize_store.storage.to_finalize_tree().unwrap().root()
            );
        }

        // Ensure removing the key succeeds.
        finalize_store.remove_key_value(&program_id, &mapping_name, &key).unwrap();
        // Ensure the program ID is still initialized.
        assert!(finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name is still initialized.
        assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure the key got removed.
        assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
        // Ensure the value returns None.
        assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Ensure removing the mapping succeeds.
        finalize_store.remove_mapping(&program_id, &mapping_name).unwrap();
        // Ensure the program ID is still initialized.
        assert!(finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name is no longer initialized.
        assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure the key is still removed.
        assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
        // Ensure the value still returns None.
        assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Ensure removing the program succeeds.
        finalize_store.remove_program(&program_id).unwrap();
        // Ensure the program ID is no longer initialized.
        assert!(!finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name is still no longer initialized.
        assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure the key is still removed.
        assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
        // Ensure the value still returns None.
        assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());
    }

    #[test]
    fn test_initialize_insert_remove() {
        // Initialize a program ID and mapping name.
        let program_id = ProgramID::<CurrentNetwork>::from_str("hello.aleo").unwrap();
        let mapping_name = Identifier::from_str("account").unwrap();

        // Initialize a new finalize store.
        let program_memory = FinalizeMemory::open(None).unwrap();
        let finalize_store = FinalizeStore::from(program_memory).unwrap();
        // Check the operations.
        check_initialize_insert_remove(&finalize_store, program_id, mapping_name);
    }

    #[test]
    fn test_initialize_update_remove() {
        // Initialize a program ID and mapping name.
        let program_id = ProgramID::<CurrentNetwork>::from_str("hello.aleo").unwrap();
        let mapping_name = Identifier::from_str("account").unwrap();

        // Initialize a new finalize store.
        let program_memory = FinalizeMemory::open(None).unwrap();
        let finalize_store = FinalizeStore::from(program_memory).unwrap();
        // Check the operations.
        check_initialize_update_remove(&finalize_store, program_id, mapping_name);
    }

    #[test]
    fn test_remove_key_value() {
        // Initialize a program ID and mapping name.
        let program_id = ProgramID::<CurrentNetwork>::from_str("hello.aleo").unwrap();
        let mapping_name = Identifier::from_str("account").unwrap();

        // Initialize a new finalize store.
        let program_memory = FinalizeMemory::open(None).unwrap();
        let finalize_store = FinalizeStore::from(program_memory).unwrap();
        // Ensure the program ID does not exist.
        assert!(!finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name does not exist.
        assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure removing an un-initialized mapping fails.
        assert!(finalize_store.remove_mapping(&program_id, &mapping_name).is_err());

        // Now, initialize the mapping.
        finalize_store.initialize_mapping(&program_id, &mapping_name).unwrap();
        // Ensure the program ID got initialized.
        assert!(finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name got initialized.
        assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());

        // Insert the list of keys and values.
        for item in 0..1000 {
            // Prepare the key and value.
            let key = Plaintext::from_str(&format!("{item}field")).unwrap();
            let value = Value::from_str(&format!("{item}u64")).unwrap();
            // Ensure the key did not get initialized.
            assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns None.
            assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());

            // Insert the key and value.
            finalize_store.insert_key_value(&program_id, &mapping_name, key.clone(), value.clone()).unwrap();
            // Ensure the program ID is still initialized.
            assert!(finalize_store.contains_program(&program_id).unwrap());
            // Ensure the mapping name is still initialized.
            assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
            // Ensure the key got initialized.
            assert!(finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns Some(value).
            assert_eq!(value, finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().unwrap());
        }
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Remove the list of keys and values.
        for item in 0..1000 {
            // Prepare the key and value.
            let key = Plaintext::from_str(&format!("{item}field")).unwrap();
            let value = Value::from_str(&format!("{item}u64")).unwrap();
            // Ensure the key is still initialized.
            assert!(finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns Some(value).
            assert_eq!(value, finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().unwrap());

            // Remove the key-value pair.
            finalize_store.remove_key_value(&program_id, &mapping_name, &key).unwrap();
            // Ensure the program ID is still initialized.
            assert!(finalize_store.contains_program(&program_id).unwrap());
            // Ensure the mapping name is still initialized.
            assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
            // Ensure the key is no longer initialized.
            assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns None.
            assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
        }
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());
    }

    #[test]
    fn test_remove_mapping() {
        // Initialize a program ID and mapping name.
        let program_id = ProgramID::<CurrentNetwork>::from_str("hello.aleo").unwrap();
        let mapping_name = Identifier::from_str("account").unwrap();

        // Initialize a new finalize store.
        let program_memory = FinalizeMemory::open(None).unwrap();
        let finalize_store = FinalizeStore::from(program_memory).unwrap();
        // Ensure the program ID does not exist.
        assert!(!finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name does not exist.
        assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure removing an un-initialized mapping fails.
        assert!(finalize_store.remove_mapping(&program_id, &mapping_name).is_err());

        // Now, initialize the mapping.
        finalize_store.initialize_mapping(&program_id, &mapping_name).unwrap();
        // Ensure the program ID got initialized.
        assert!(finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name got initialized.
        assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());

        // Insert the list of keys and values.
        for item in 0..1000 {
            // Prepare the key and value.
            let key = Plaintext::from_str(&format!("{item}field")).unwrap();
            let value = Value::from_str(&format!("{item}u64")).unwrap();
            // Ensure the key did not get initialized.
            assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns None.
            assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());

            // Insert the key and value.
            finalize_store.insert_key_value(&program_id, &mapping_name, key.clone(), value.clone()).unwrap();
            // Ensure the program ID is still initialized.
            assert!(finalize_store.contains_program(&program_id).unwrap());
            // Ensure the mapping name is still initialized.
            assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
            // Ensure the key got initialized.
            assert!(finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns Some(value).
            assert_eq!(value, finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().unwrap());
        }
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Remove the mapping.
        finalize_store.remove_mapping(&program_id, &mapping_name).unwrap();
        // Ensure the program ID is still initialized.
        assert!(finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name is no longer initialized.
        assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Check the list of keys and values.
        for item in 0..1000 {
            // Prepare the key.
            let key = Plaintext::from_str(&format!("{item}field")).unwrap();

            // Ensure the key is no longer initialized.
            assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns None.
            assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
        }
    }

    #[test]
    fn test_remove_program() {
        // Initialize a program ID and mapping name.
        let program_id = ProgramID::<CurrentNetwork>::from_str("hello.aleo").unwrap();
        let mapping_name = Identifier::from_str("account").unwrap();

        // Initialize a new finalize store.
        let program_memory = FinalizeMemory::open(None).unwrap();
        let finalize_store = FinalizeStore::from(program_memory).unwrap();
        // Ensure the program ID does not exist.
        assert!(!finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name does not exist.
        assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure removing an un-initialized mapping fails.
        assert!(finalize_store.remove_mapping(&program_id, &mapping_name).is_err());

        // Now, initialize the mapping.
        finalize_store.initialize_mapping(&program_id, &mapping_name).unwrap();
        // Ensure the program ID got initialized.
        assert!(finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name got initialized.
        assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());

        // Insert the list of keys and values.
        for item in 0..1000 {
            // Prepare the key and value.
            let key = Plaintext::from_str(&format!("{item}field")).unwrap();
            let value = Value::from_str(&format!("{item}u64")).unwrap();
            // Ensure the key did not get initialized.
            assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns None.
            assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());

            // Insert the key and value.
            finalize_store.insert_key_value(&program_id, &mapping_name, key.clone(), value.clone()).unwrap();
            // Ensure the program ID is still initialized.
            assert!(finalize_store.contains_program(&program_id).unwrap());
            // Ensure the mapping name is still initialized.
            assert!(finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
            // Ensure the key got initialized.
            assert!(finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns Some(value).
            assert_eq!(value, finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().unwrap());
        }
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Remove the program.
        finalize_store.remove_program(&program_id).unwrap();
        // Ensure the program ID is no longer initialized.
        assert!(!finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name is no longer initialized.
        assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure that the storage tree is updated correctly.
        assert_eq!(finalize_store.current_storage_root(), *finalize_store.storage.to_finalize_tree().unwrap().root());

        // Check the list of keys and values.
        for item in 0..1000 {
            // Prepare the key.
            let key = Plaintext::from_str(&format!("{item}field")).unwrap();

            // Ensure the key is no longer initialized.
            assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns None.
            assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
        }
    }

    #[test]
    fn test_must_initialize_first() {
        // Initialize a program ID and mapping name.
        let program_id = ProgramID::<CurrentNetwork>::from_str("hello.aleo").unwrap();
        let mapping_name = Identifier::from_str("account").unwrap();

        // Initialize a new finalize store.
        let program_memory = FinalizeMemory::open(None).unwrap();
        let finalize_store = FinalizeStore::from(program_memory).unwrap();
        // Ensure the program ID does not exist.
        assert!(!finalize_store.contains_program(&program_id).unwrap());
        // Ensure the mapping name does not exist.
        assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
        // Ensure removing an un-initialized mapping fails.
        assert!(finalize_store.remove_mapping(&program_id, &mapping_name).is_err());

        {
            // Ensure inserting a (key, value) before initializing the mapping fails.
            let key = Plaintext::from_str("123456789field").unwrap();
            let value = Value::from_str("987654321u128").unwrap();
            assert!(finalize_store.insert_key_value(&program_id, &mapping_name, key.clone(), value).is_err());

            // Ensure the program ID did not get initialized.
            assert!(!finalize_store.contains_program(&program_id).unwrap());
            // Ensure the mapping name did not get initialized.
            assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
            // Ensure the key did not get initialized.
            assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns None.
            assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
            // Ensure removing an un-initialized key fails.
            assert!(finalize_store.remove_key_value(&program_id, &mapping_name, &key).is_err());
            // Ensure removing an un-initialized mapping fails.
            assert!(finalize_store.remove_mapping(&program_id, &mapping_name).is_err());
            // Ensure that the storage tree is updated correctly.
            assert_eq!(
                finalize_store.current_storage_root(),
                *finalize_store.storage.to_finalize_tree().unwrap().root()
            );
        }
        {
            // Ensure updating a (key, value) before initializing the mapping fails.
            let key = Plaintext::from_str("987654321field").unwrap();
            let value = Value::from_str("123456789u128").unwrap();
            assert!(finalize_store.update_key_value(&program_id, &mapping_name, key.clone(), value).is_err());

            // Ensure the program ID did not get initialized.
            assert!(!finalize_store.contains_program(&program_id).unwrap());
            // Ensure the mapping name did not get initialized.
            assert!(!finalize_store.contains_mapping(&program_id, &mapping_name).unwrap());
            // Ensure the key did not get initialized.
            assert!(!finalize_store.contains_key(&program_id, &mapping_name, &key).unwrap());
            // Ensure the value returns None.
            assert!(finalize_store.get_value(&program_id, &mapping_name, &key).unwrap().is_none());
            // Ensure removing an un-initialized key fails.
            assert!(finalize_store.remove_key_value(&program_id, &mapping_name, &key).is_err());
            // Ensure removing an un-initialized mapping fails.
            assert!(finalize_store.remove_mapping(&program_id, &mapping_name).is_err());
        }

        // Ensure finalize storage still behaves correctly after the above operations.
        check_initialize_insert_remove(&finalize_store, program_id, mapping_name);
        check_initialize_update_remove(&finalize_store, program_id, mapping_name);
    }
}
