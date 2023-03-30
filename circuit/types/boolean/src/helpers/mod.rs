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

use super::*;

pub mod adder;
pub mod from_bits;
pub mod subtractor;
pub mod to_bits;

impl<E: Environment> Boolean<E> {
    /// Returns `true` if `circuit_bits_le <= console_bits_le`.
    /// This function assumes the inputs are in **little-endian** representation.
    pub fn is_less_than_or_equal(circuit_bits_le: &[Boolean<E>], console_bits_le: &[bool]) -> Boolean<E> {
        // Ensure the length matches.
        if circuit_bits_le.len() != console_bits_le.len() {
            E::halt(format!("Mismatching length of bits ({} != {})", circuit_bits_le.len(), console_bits_le.len()))
        }

        // Compute `!(console_bits_le < circuit_bits_le)`, equivalent to `console_bits_le >= circuit_bits_le`.
        !console_bits_le.iter().zip_eq(circuit_bits_le).fold(Boolean::constant(false), |rest_is_less, (this, that)| {
            if *this { that.bitand(&rest_is_less) } else { that.bitor(&rest_is_less) }
        })
    }

    /// Asserts that `circuit_bits_le <= console_bits_le`.
    /// This function assumes the inputs are in **little-endian** representation.
    pub fn assert_less_than_or_equal(circuit_bits_le: &[Boolean<E>], console_bits_le: &[bool]) {
        // Compute `!(console_bits_le < circuit_bits_le)`, equivalent to `console_bits_le >= circuit_bits_le`.
        let is_less_than_or_equal = Boolean::is_less_than_or_equal(circuit_bits_le, console_bits_le);
        // Assert that `circuit_bits_le <= console_bits_le`.
        E::assert(is_less_than_or_equal);
    }
}
