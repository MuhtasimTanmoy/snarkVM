// Copyright (C) 2019-2023 Aleo Systems Inc.
// This file is part of the snarkVM library.

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at:
// http://www.apache.org/licenses/LICENSE-2.0

// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::QueryTrait;
use console::{
    network::prelude::*,
    program::{ProgramID, StatePath},
    types::Field,
};
use ledger_store::{BlockStorage, BlockStore};
use synthesizer_program::Program;

#[derive(Clone)]
pub enum Query<N: Network, B: BlockStorage<N>> {
    /// The block store from the VM.
    VM(BlockStore<N, B>),
    /// The base URL of the node.
    REST(String),
    /// The hardcoded state root and state path.
    CONST(N::StateRoot, StatePath<N>),
}

impl<N: Network, B: BlockStorage<N>> From<BlockStore<N, B>> for Query<N, B> {
    fn from(block_store: BlockStore<N, B>) -> Self {
        Self::VM(block_store)
    }
}

impl<N: Network, B: BlockStorage<N>> From<&BlockStore<N, B>> for Query<N, B> {
    fn from(block_store: &BlockStore<N, B>) -> Self {
        Self::VM(block_store.clone())
    }
}

impl<N: Network, B: BlockStorage<N>> From<String> for Query<N, B> {
    fn from(url: String) -> Self {
        Self::REST(url)
    }
}

impl<N: Network, B: BlockStorage<N>> From<&String> for Query<N, B> {
    fn from(url: &String) -> Self {
        Self::REST(url.to_string())
    }
}

impl<N: Network, B: BlockStorage<N>> From<&str> for Query<N, B> {
    fn from(url: &str) -> Self {
        Self::REST(url.to_string())
    }
}

impl<N: Network, B: BlockStorage<N>> From<(N::StateRoot, StatePath<N>)> for Query<N, B> {
    fn from((state_root, state_path): (N::StateRoot, StatePath<N>)) -> Self {
        Self::CONST(state_root, state_path)
    }
}

impl<N: Network, B: BlockStorage<N>> From<&(N::StateRoot, StatePath<N>)> for Query<N, B> {
    fn from((state_root, state_path): &(N::StateRoot, StatePath<N>)) -> Self {
        Self::CONST(*state_root, state_path.clone())
    }
}

#[cfg_attr(feature = "async", async_trait(?Send))]
impl<N: Network, B: BlockStorage<N>> QueryTrait<N> for Query<N, B> {
    /// Returns the current state root.
    fn current_state_root(&self) -> Result<N::StateRoot> {
        match self {
            Self::VM(block_store) => Ok(block_store.current_state_root()),
            Self::REST(url) => match N::ID {
                3 => Ok(Self::get_request(&format!("{url}/testnet3/latest/stateRoot"))?.into_json()?),
                _ => bail!("Unsupported network ID in inclusion query"),
            },
            Self::CONST(state_root, _) => Ok(*state_root),
        }
    }

    /// Returns the current state root.
    #[cfg(feature = "async")]
    async fn current_state_root_async(&self) -> Result<N::StateRoot> {
        match self {
            Self::VM(block_store) => Ok(block_store.current_state_root()),
            Self::REST(url) => match N::ID {
                3 => Ok(Self::get_request_async(&format!("{url}/testnet3/latest/stateRoot")).await?.json().await?),
                _ => bail!("Unsupported network ID in inclusion query"),
            },
            Self::CONST(state_root, _) => Ok(*state_root),
        }
    }

    /// Returns a state path for the given `commitment`.
    fn get_state_path_for_commitment(&self, commitment: &Field<N>) -> Result<StatePath<N>> {
        match self {
            Self::VM(block_store) => block_store.get_state_path_for_commitment(commitment),
            Self::REST(url) => match N::ID {
                3 => Ok(Self::get_request(&format!("{url}/testnet3/statePath/{commitment}"))?.into_json()?),
                _ => bail!("Unsupported network ID in inclusion query"),
            },
            Self::CONST(_, state_path) => Ok(state_path.clone()),
        }
    }

    /// Returns a state path for the given `commitment`.
    #[cfg(feature = "async")]
    async fn get_state_path_for_commitment_async(&self, commitment: &Field<N>) -> Result<StatePath<N>> {
        match self {
            Self::VM(block_store) => block_store.get_state_path_for_commitment(commitment),
            Self::REST(url) => match N::ID {
                3 => {
                    Ok(Self::get_request_async(&format!("{url}/testnet3/statePath/{commitment}")).await?.json().await?)
                }
                _ => bail!("Unsupported network ID in inclusion query"),
            },
            Self::CONST(_, state_path) => Ok(state_path.clone()),
        }
    }
}

impl<N: Network, B: BlockStorage<N>> Query<N, B> {
    /// Returns the program for the given program ID.
    pub fn get_program(&self, program_id: &ProgramID<N>) -> Result<Program<N>> {
        match self {
            Self::VM(block_store) => {
                block_store.get_program(program_id)?.ok_or_else(|| anyhow!("Program {program_id} not found in storage"))
            }
            Self::REST(url) => match N::ID {
                3 => Ok(Self::get_request(&format!("{url}/testnet3/program/{program_id}"))?.into_json()?),
                _ => bail!("Unsupported network ID in inclusion query"),
            },
            Self::CONST(..) => bail!("Cannot get a program from `CONST` query"),
        }
    }

    /// Returns the program for the given program ID.
    #[cfg(feature = "async")]
    pub async fn get_program_async(&self, program_id: &ProgramID<N>) -> Result<Program<N>> {
        match self {
            Self::VM(block_store) => {
                block_store.get_program(program_id)?.ok_or_else(|| anyhow!("Program {program_id} not found in storage"))
            }
            Self::REST(url) => match N::ID {
                3 => Ok(Self::get_request_async(&format!("{url}/testnet3/program/{program_id}")).await?.json().await?),
                _ => bail!("Unsupported network ID in inclusion query"),
            },
            Self::CONST(..) => bail!("Cannot get a program from `CONST` query"),
        }
    }

    /// Performs a GET request to the given URL.
    fn get_request(url: &str) -> Result<ureq::Response> {
        let response = ureq::get(url).call()?;
        if response.status() == 200 { Ok(response) } else { bail!("Failed to fetch from {url}") }
    }

    /// Performs a GET request to the given URL.
    #[cfg(feature = "async")]
    async fn get_request_async(url: &str) -> Result<reqwest::Response> {
        let response = reqwest::get(url).await?;
        if response.status() == 200 { Ok(response) } else { bail!("Failed to fetch from {url}") }
    }
}
