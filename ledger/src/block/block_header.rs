// Copyright (C) 2019-2021 Aleo Systems Inc.
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

use crate::{BlockHeaderHash, MerkleRootHash, PedersenMerkleRootHash, ProofOfSuccinctWork};
use snarkvm_algorithms::crh::{double_sha256, sha256d_to_u64};
use snarkvm_utilities::{FromBytes, ToBytes};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Result as IoResult, Write},
    mem::size_of,
};

const HEADER_SIZE: usize = {
    BlockHeaderHash::size()
        + MerkleRootHash::size()
        + PedersenMerkleRootHash::size()
        + ProofOfSuccinctWork::size()
        + size_of::<i64>()
        + size_of::<u64>()
        + size_of::<u32>()
};

/// Block header.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlockHeader {
    /// Hash of the previous block - 32 bytes
    pub previous_block_hash: BlockHeaderHash,
    /// Merkle root representing the transactions in the block - 32 bytes
    pub merkle_root_hash: MerkleRootHash,
    /// Merkle root of the transactions in the block using a Pedersen hash - 32 bytes
    pub pedersen_merkle_root_hash: PedersenMerkleRootHash,
    /// Proof of Succinct Work
    pub proof: ProofOfSuccinctWork,
    /// The block timestamp is a Unix epoch time (UTC) when the miner
    /// started hashing the header (according to the miner). - 8 bytes
    pub time: i64,
    /// Proof of work algorithm difficulty target for this block - 8 bytes
    pub difficulty_target: u64,
    /// Nonce for solving the PoW puzzle - 4 bytes
    pub nonce: u32,
}

impl BlockHeader {
    /// Returns `true` if the block header is uniquely a genesis block header.
    pub fn is_genesis(&self) -> bool {
        // Ensure the timestamp in the genesis block is 0.
        self.time == 0
            // Ensure the previous block hash in the genesis block is 0.
            || self.previous_block_hash == BlockHeaderHash([0u8; 32])
    }

    pub fn get_hash(&self) -> Result<BlockHeaderHash> {
        let serialized = self.to_bytes_le()?;
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&double_sha256(&serialized));

        Ok(BlockHeaderHash(hash))
    }

    pub fn to_difficulty_hash(&self) -> u64 {
        sha256d_to_u64(&self.proof.0[..])
    }

    pub const fn size() -> usize {
        HEADER_SIZE
    }
}

impl ToBytes for BlockHeader {
    #[inline]
    fn write_le<W: Write>(&self, mut writer: W) -> IoResult<()> {
        self.previous_block_hash.0.write_le(&mut writer)?;
        self.merkle_root_hash.0.write_le(&mut writer)?;
        self.pedersen_merkle_root_hash.0.write_le(&mut writer)?;
        self.proof.write_le(&mut writer)?;
        self.time.to_le_bytes().write_le(&mut writer)?;
        self.difficulty_target.to_le_bytes().write_le(&mut writer)?;
        self.nonce.to_le_bytes().write_le(&mut writer)
    }
}

impl FromBytes for BlockHeader {
    #[inline]
    fn read_le<R: Read>(mut reader: R) -> IoResult<Self> {
        let previous_block_hash = <[u8; 32]>::read_le(&mut reader)?;
        let merkle_root_hash = <[u8; 32]>::read_le(&mut reader)?;
        let pedersen_merkle_root_hash = <[u8; 32]>::read_le(&mut reader)?;
        let proof = ProofOfSuccinctWork::read_le(&mut reader)?;
        let time = <[u8; 8]>::read_le(&mut reader)?;
        let difficulty_target = <[u8; 8]>::read_le(&mut reader)?;
        let nonce = <[u8; 4]>::read_le(&mut reader)?;

        Ok(Self {
            previous_block_hash: BlockHeaderHash(previous_block_hash),
            merkle_root_hash: MerkleRootHash(merkle_root_hash),
            time: i64::from_le_bytes(time),
            difficulty_target: u64::from_le_bytes(difficulty_target),
            nonce: u32::from_le_bytes(nonce),
            pedersen_merkle_root_hash: PedersenMerkleRootHash(pedersen_merkle_root_hash),
            proof,
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn test_block_header_serialization() {
        let block_header = BlockHeader {
            previous_block_hash: BlockHeaderHash([0u8; 32]),
            merkle_root_hash: MerkleRootHash([0u8; 32]),
            pedersen_merkle_root_hash: PedersenMerkleRootHash([0u8; 32]),
            proof: ProofOfSuccinctWork([0u8; ProofOfSuccinctWork::size()]),
            time: Utc::now().timestamp(),
            difficulty_target: 0u64,
            nonce: 0u32,
        };

        let mut serialized = vec![];
        block_header.write_le(&mut serialized).unwrap();
        let deserialized = BlockHeader::read_le(&serialized[..]).unwrap();

        assert_eq!(&serialized[..], &bincode::serialize(&block_header).unwrap()[..]);
        assert_eq!(block_header, deserialized);
    }
}
