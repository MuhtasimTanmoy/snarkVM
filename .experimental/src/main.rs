// Copyright (C) 2019-2022 Aleo Systems Inc.
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

use console::{
    account::{Address, PrivateKey, Signature, ViewKey},
    collections::merkle_tree::MerkleTree,
    network::{Network, Testnet3},
    program::{Ciphertext, Data, Randomizer, Record, State},
};
use snarkvm_curves::ProjectiveCurve;
use snarkvm_experimental::{input, output, snark};
use snarkvm_fields::{PrimeField, Zero};
use snarkvm_utilities::{CryptoRng, Rng, ToBits, UniformRand};

use anyhow::{bail, Error, Result};
use core::panic::{RefUnwindSafe, UnwindSafe};
use rand::prelude::ThreadRng;
use snarkvm_algorithms::snark::marlin::Proof;
use snarkvm_curves::AffineCurve;
use std::{thread, time::Instant};

struct Input<N: Network> {
    /// The serial number of the input record.
    serial_number: N::Field,
    /// The (randomized) balance commitment (i.e. `bcm := Commit(balance, r_bcm + k_bcm)`).
    bcm: N::Affine,
}

impl<N: Network> Input<N> {
    /// Initializes a new `Input` for a transition.
    pub const fn new(serial_number: N::Field, bcm: N::Affine) -> Self {
        Self { serial_number, bcm }
    }

    /// Returns the serial number of the input record.
    pub const fn serial_number(&self) -> N::Field {
        self.serial_number
    }

    /// Returns the balance commitment for the input record.
    pub const fn bcm(&self) -> N::Affine {
        self.bcm
    }
}

struct Output<N: Network> {
    /// The output record.
    record: Record<N>,
}

impl<N: Network> Output<N> {
    /// Initializes a new `Output` for a transition.
    pub const fn new(record: Record<N>) -> Self {
        Self { record }
    }

    /// Returns the output record.
    pub const fn record(&self) -> &Record<N> {
        &self.record
    }

    /// Returns the balance commitment for the output record.
    pub const fn bcm(&self) -> N::Affine {
        self.record.bcm()
    }

    /// Returns the output commitment.
    pub fn to_commitment(&self) -> Result<N::Field> {
        self.record.to_commitment()
    }
}

pub struct Transition<N: Network> {
    /// The transition inputs.
    inputs: Vec<Input<N>>,
    /// The transition outputs.
    outputs: Vec<Output<N>>,
    /// The transition input proofs.
    input_proofs: Vec<Proof<snarkvm_curves::bls12_377::Bls12_377>>,
    /// The transition output proofs.
    output_proofs: Vec<Proof<snarkvm_curves::bls12_377::Bls12_377>>,
    /// The address commitment (i.e. `acm := Commit(caller, r_acm)`).
    acm: N::Field,
    /// The fee (i.e. `fee := Σ balance_in - Σ balance_out`).
    fee: i64,
}

impl<N: Network> Transition<N> {
    /// Returns `true` if the transition is valid.
    pub fn verify(&self) -> bool {
        // self.
        true
    }

    /// Returns the serial numbers in the transition.
    pub fn serial_numbers(&self) -> Vec<N::Field> {
        self.inputs.iter().map(Input::serial_number).collect::<Vec<_>>()
    }

    /// Returns the commitments in the transition.
    pub fn to_commitments(&self) -> Result<Vec<N::Field>> {
        self.outputs.iter().map(Output::to_commitment).collect::<Result<Vec<_>>>()
    }

    /// Returns the fee commitment of this transition, where:
    ///   - `fcm := Σ bcm_in - Σ bcm_out - Commit(fee, 0) = Commit(0, r_fcm)`
    pub fn fcm(&self) -> Result<N::Affine> {
        let mut fcm = N::Projective::zero();
        // Add the input balance commitments.
        self.inputs.iter().for_each(|input| fcm += input.bcm().to_projective());
        // Subtract the output balance commitments.
        self.outputs.iter().for_each(|output| fcm -= output.bcm().to_projective());
        // Subtract the fee to get the fee commitment.
        let fcm = match self.fee.is_positive() {
            true => fcm - N::commit_ped64(&self.fee.abs().to_bits_le(), &N::Scalar::zero())?.to_projective(),
            false => fcm + N::commit_ped64(&self.fee.abs().to_bits_le(), &N::Scalar::zero())?.to_projective(),
        };
        // Return the fee commitment.
        Ok(fcm.to_affine())
    }
}

pub struct Transaction<N: Network> {
    /// The network ID.
    network: u16,
    /// The ordered list of transitions in this transaction.
    transitions: Vec<Transition<N>>,
}

impl<N: Network> Transaction<N> {
    /// Returns the transitions in the transaction.
    pub fn transitions(&self) -> &Vec<Transition<N>> {
        &self.transitions
    }
}

/// Returns the address commitment as `bcm := Commit(caller, r_acm)`.
fn acm<A: circuit::Aleo, R: Rng + CryptoRng>(
    caller: &Address<A::Network>,
    rng: &mut R,
) -> Result<(A::BaseField, A::ScalarField)> {
    // TODO (howardwu): Domain separator.
    let r_acm = UniformRand::rand(rng);
    // TODO (howardwu): Add a to_bits impl for caller.
    let acm = A::Network::commit_bhp256(&(*caller).to_x_coordinate().to_bits_le(), &r_acm)?;
    Ok((acm, r_acm))
}

/// Returns the (randomized) balance commitment as `bcm := Commit(balance, k_bcm + r_bcm)`.
fn bcm<A: circuit::Aleo, R: Rng + CryptoRng>(
    bcm: A::Affine,
    record_view_key: A::BaseField,
    rng: &mut R,
) -> Result<(A::Affine, A::ScalarField, A::ScalarField)> {
    // Compute the randomizer for the balance commitment (i.e. HashToScalar(G^r^view_key));
    let r_bcm = A::Network::hash_to_scalar_psd2(&[A::Network::bcm_domain(), record_view_key])?;
    // TODO (howardwu): Domain separator.
    let k_bcm = UniformRand::rand(rng);
    let bcm = bcm.to_projective() + A::Network::commit_ped64(&0u64.to_bits_le(), &k_bcm)?.to_projective();
    Ok((bcm.to_affine(), r_bcm, k_bcm))
}

/// Returns the fee commitment `fcm` and fee randomizer `r_fcm`, where:
///   - `fcm := Σ bcm_in - Σ bcm_out - Commit(fee, 0) = Commit(0, r_fcm)`
///   - `r_fcm := Σ r_in - Σ r_out`.
fn fcm<A: circuit::Aleo>(r_in: &[A::ScalarField], r_out: &[A::ScalarField]) -> Result<(A::Affine, A::ScalarField)> {
    // Compute the fee randomizer.
    let mut r_fcm = A::ScalarField::zero();
    r_in.iter().for_each(|r| r_fcm += r);
    r_out.iter().for_each(|r| r_fcm -= r);
    // Compute the fee commitment.
    let fcm = A::Network::commit_ped64(&0u64.to_bits_le(), &r_fcm)?;
    Ok((fcm, r_fcm))
}

// // TODO (howardwu): Enforce 2^52.
// let difference = b_in as i64 - b_out as i64 - fee;
// let r_bcm = r_in - r_out;
// // Compute bcm := G^(b_in - b_out - fee) H^(r_in - r_out).
// let bcm = A::Network::commit_ped64(&difference.abs().to_bits_le(), &r_bcm)?;
// // Ensure `bcm` == `G^0 H^(r_in - r_out)`.
// assert_eq!(bcm, );

/// Transition: 0 -> 1
fn mint<A: circuit::Aleo, R: Rng + CryptoRng>(
    rng: &mut R,
    caller_view_key: &ViewKey<A::Network>,
    amount: u64,
) -> Result<Transaction<A::Network>>
where
    A::BaseField: UnwindSafe + RefUnwindSafe,
    A::ScalarField: UnwindSafe + RefUnwindSafe,
    A::Affine: UnwindSafe + RefUnwindSafe,
{
    // Initialize the caller address.
    let caller_address = Address::try_from(caller_view_key)?;

    // Initialize the randomizer, which is bound to the account of the **sender**.
    let randomizer = Randomizer::prove(caller_view_key, &[], 0, rng)?;

    // Initialize a coinbase.
    let (state, record) = {
        let program = <A::Network as Network>::Field::zero(); // TODO: Hardcode this option in the Network trait.
        let process = <A::Network as Network>::Field::zero(); // TODO: Hardcode this option in the Network trait.
        let owner = caller_address;
        let balance = amount;
        let data = <A::Network as Network>::Field::zero(); // TODO: Hardcode this option in the Network trait.

        let state = State::new(program, process, owner, balance, data, &randomizer);
        let record = state.encrypt(&randomizer)?;
        (state, record)
    };

    // Compute the address commitment.
    let (acm, r_acm) = acm::<A, R>(&caller_address, rng)?;

    // Compute the record view key.
    let record_view_key = record.to_record_view_key(caller_view_key);
    // Compute the randomizer for the balance commitment (i.e. HashToScalar(G^r^view_key));
    let r_bcm = A::Network::hash_to_scalar_psd2(&[A::Network::bcm_domain(), record_view_key])?;
    // Compute the fee commitment.
    let (fcm, r_fcm) = fcm::<A>(&[], &[r_bcm])?;

    let process = std::panic::catch_unwind(|| {
        // Set the output index to 0.
        let output_index = 0u16;
        // Compute the serial numbers digest.
        let serial_numbers_digest = A::Network::hash_bhp1024(&[])?;

        let public = output::Public::<A>::from(output_index, record.clone(), serial_numbers_digest, acm, fcm);
        let private = output::Private::<A>::from(state, randomizer, caller_address, r_acm, r_fcm);
        output::OutputCircuit::from(public, private)?.execute();
        println!("Is satisfied? {} ({} constraints)", A::is_satisfied(), A::num_constraints());

        let (num_constant, num_public, num_private, num_constraints, num_gates) = A::count();
        println!(
            "Count(Constant: {num_constant}, Public: {num_public}, Private: {num_private}, Constraints: {num_constraints}, Gates: {num_gates})"
        );

        let timer = Instant::now();
        let assignment = circuit::Circuit::eject();
        println!("Convert to assignment: {} ms", timer.elapsed().as_millis());

        let proof = snark::execute(assignment)?;
        let transition = Transition {
            inputs: vec![],
            outputs: vec![Output::new(record)],
            input_proofs: vec![],
            output_proofs: vec![proof],
            acm,
            fee: -(amount as i64),
        };
        assert_eq!(fcm, transition.fcm()?);

        // Set the network ID to 0.
        let network = 0u16;
        let transaction = Transaction { network, transitions: vec![transition] };

        Ok::<_, Error>(transaction)
    });

    match process {
        Ok(Ok(transaction)) => Ok(transaction),
        Ok(Err(error)) => bail!("{:?}", error),
        Err(_) => bail!("Thread failed"),
    }
}

/// Transition: 1 -> 0
fn burn<A: circuit::Aleo, R: Rng + CryptoRng>(rng: &mut R) -> Result<Transaction<A::Network>>
where
    A::BaseField: UnwindSafe + RefUnwindSafe,
    A::ScalarField: UnwindSafe + RefUnwindSafe,
    A::Affine: UnwindSafe + RefUnwindSafe,
{
    // Initialize a new caller account.
    let caller_private_key = PrivateKey::<A::Network>::new(rng)?;
    let caller_view_key = ViewKey::try_from(&caller_private_key)?;
    let caller_address = Address::try_from(&caller_private_key)?;

    // Generate a prior coinbase transaction.
    let transaction = mint::<A, R>(rng, &caller_view_key, 100u64)?;

    // Retrieve the coinbase record.
    let record = transaction.transitions()[0].outputs[0].record();

    // Initialize a program tree with the coinbase record.
    let program = A::Network::merkle_tree_bhp::<32>(&[record.to_bits_le()])?; // TODO: Add test that record ID matches in tree.
    // Compute a Merkle path for the coinbase record.
    let path = program.prove(0, &record.to_bits_le())?;
    // Retrieve the Merkle root.
    let root = program.root();

    // Compute the record view key.
    let record_view_key = record.to_record_view_key(&caller_view_key);

    // Compute the serial number.
    let serial_number = record.to_serial_number(&caller_private_key, rng)?;
    // Compute the signature for the serial number.
    let signature = Signature::sign(&caller_private_key, &[*serial_number.value()], rng)?;

    // Compute the address commitment.
    let (acm, r_acm) = acm::<A, R>(&caller_address, rng)?;

    // fn bcm<A: circuit::Aleo>(
    //     b_in: u64,
    //     r_in: A::ScalarField,
    //     b_out: u64,
    //     r_out: A::ScalarField,
    //     fee: i64,
    // ) -> Result<(A::BaseField, A::ScalarField)> {
    //     // TODO (howardwu): Enforce 2^52.
    //     let difference = b_in as i64 - b_out as i64 - fee;
    //     let r_bcm = r_in - r_out;
    //     // Compute bcm := G^(b_in - b_out - fee) H^(r_in - r_out).
    //     let bcm = A::Network::commit_ped64(&difference.abs().to_bits_le(), &r_bcm)?;
    //     // Ensure `bcm` == `G^0 H^(r_in - r_out)`.
    //     assert_eq!(bcm, A::Network::commit_ped64(&0u64.to_bits_le(), &r_bcm)?);
    //     Ok((bcm, r_bcm))
    // }

    // let fee = -(state.balance() as i64);
    // let (bcm, r_bcm) = bcm::<A>(0, A::ScalarField::zero(), state.balance(), r_bcm, fee)?;

    // // Compute the record view key.
    // let record_view_key = record.to_record_view_key(&caller_view_key);
    // // Compute the randomizer for the balance commitment (i.e. HashToScalar(G^r^view_key));
    // let r_bcm = A::Network::hash_to_scalar_psd2(&[A::Network::bcm_domain(), record_view_key])?;

    // Decrypt the record into program state.
    let state = record.decrypt_symmetric(&record_view_key)?;
    let fee = state.balance() as i64;

    // Compute the balance commitment.
    let (bcm, r_bcm, k_bcm) = bcm::<A, R>(record.bcm(), record_view_key, rng)?;

    // Compute the fee commitment.
    let (fcm, r_fcm) = fcm::<A>(&[r_bcm + k_bcm], &[])?;

    let process = std::panic::catch_unwind(|| {
        let public = input::Public::<A>::from(*root, *serial_number.value(), acm, bcm, fcm);
        let private = input::Private::<A>::from(
            record_view_key,
            record.clone(),
            serial_number.clone(),
            signature,
            r_acm,
            k_bcm,
            r_fcm,
        );
        let input_circuit = input::InputCircuit::from(public, private)?;
        input_circuit.execute();

        let (num_constant, num_public, num_private, num_constraints, num_gates) = A::count();
        println!(
            "Count(Constant: {num_constant}, Public: {num_public}, Private: {num_private}, Constraints: {num_constraints}, Gates: {num_gates})"
        );

        let timer = Instant::now();
        let assignment = circuit::Circuit::eject();
        println!("Convert to assignment: {} ms", timer.elapsed().as_millis());

        let proof = snark::execute(assignment)?;
        let transition = Transition {
            inputs: vec![Input::new(*serial_number.value(), bcm)],
            outputs: vec![],
            input_proofs: vec![proof],
            output_proofs: vec![],
            acm,
            fee,
        };
        assert_eq!(fcm, transition.fcm()?);

        // Set the network ID to 0.
        let network = 0u16;
        let transaction = Transaction { network, transitions: vec![transition] };

        Ok::<_, Error>(transaction)
    });

    match process {
        Ok(Ok(transaction)) => Ok(transaction),
        Ok(Err(error)) => bail!("{:?}", error),
        Err(_) => bail!("Thread failed"),
    }
}

fn main() -> Result<()> {
    let mut rng = rand::thread_rng();

    // // Initialize a new caller account.
    // let caller_private_key = PrivateKey::<<circuit::AleoV0 as circuit::Environment>::Network>::new(&mut rng)?;
    // let caller_view_key = ViewKey::try_from(&caller_private_key)?;
    // let caller_address = Address::try_from(&caller_private_key)?;
    //
    // let transaction = mint::<circuit::AleoV0, ThreadRng>(&mut rng, &caller_view_key, 100u64)?;

    let transaction = burn::<circuit::AleoV0, ThreadRng>(&mut rng)?;

    Ok(())
}
