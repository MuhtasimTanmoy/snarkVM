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

use super::*;

impl<N: Network> Process<N> {
    /// Verifies the given execution is valid.
    /// Note: This does *not* check that the global state root exists in the ledger.
    #[inline]
    pub fn verify_execution(&self, execution: &Execution<N>) -> Result<()> {
        let timer = timer!("Process::verify_execution");

        // Ensure the execution contains transitions.
        ensure!(!execution.is_empty(), "There are no transitions in the execution");

        // Ensure the number of transitions matches the program function.
        let locator = {
            // Retrieve the transition (without popping it).
            let transition = execution.peek()?;
            // Retrieve the stack.
            let stack = self.get_stack(transition.program_id())?;
            // Ensure the number of calls matches the number of transitions.
            let number_of_calls = stack.get_number_of_calls(transition.function_name())?;
            ensure!(
                number_of_calls == execution.len(),
                "The number of transitions in the execution is incorrect. Expected {number_of_calls}, but found {}",
                execution.len()
            );
            // Output the locator of the main function.
            Locator::new(*transition.program_id(), *transition.function_name()).to_string()
        };
        lap!(timer, "Verify the number of transitions");

        // Construct the call graph of the execution.
        let call_graph = self.construct_call_graph(execution)?;
        // Construct the inverted call graph of the execution. This is a mapping from the transition ID of a callee transition to the transition ID of its caller.
        let inverted_call_graph = Self::invert_call_graph(&call_graph);
        // Construct a mapping of transition IDs to program IDs.
        let transition_program_ids = execution
            .transitions()
            .map(|transition| (*transition.id(), *transition.program_id()))
            .collect::<HashMap<_, _>>();

        // Initialize a map of verifying keys to public inputs.
        let mut verifier_inputs = HashMap::new();

        // Initialize a map of transition IDs to references of the transition.
        let mut transition_map = HashMap::new();

        // Verify each transition.
        for transition in execution.transitions() {
            #[cfg(debug_assertions)]
            println!("Verifying transition for {}/{}...", transition.program_id(), transition.function_name());
            // Debug-mode only, as the `Transition` constructor recomputes the transition ID at initialization.
            debug_assert_eq!(
                **transition.id(),
                N::hash_bhp512(&(transition.to_root()?, *transition.tcm()).to_bits_le())?,
                "The transition ID is incorrect"
            );

            // Ensure the transition is not a fee transition.
            let is_fee_transition = transition.is_fee_private() || transition.is_fee_public();
            ensure!(!is_fee_transition, "Fee transitions are not allowed in executions");
            // Ensure the number of inputs is within the allowed range.
            ensure!(transition.inputs().len() <= N::MAX_INPUTS, "Transition exceeded maximum number of inputs");
            // Ensure the number of outputs is within the allowed range.
            ensure!(transition.outputs().len() <= N::MAX_OUTPUTS, "Transition exceeded maximum number of outputs");

            // Compute the function ID as `Hash(network_id, program_id, function_name)`.
            let function_id = N::hash_bhp1024(
                &(
                    U16::<N>::new(N::ID),
                    transition.program_id().name(),
                    transition.program_id().network(),
                    transition.function_name(),
                )
                    .to_bits_le(),
            )?;

            // Ensure each input is valid.
            if transition
                .inputs()
                .iter()
                .enumerate()
                .any(|(index, input)| !input.verify(function_id, transition.tcm(), index))
            {
                bail!("Failed to verify a transition input")
            }
            lap!(timer, "Verify the inputs");

            // Ensure each output is valid.
            let num_inputs = transition.inputs().len();
            if transition
                .outputs()
                .iter()
                .enumerate()
                .any(|(index, output)| !output.verify(function_id, transition.tcm(), num_inputs + index))
            {
                bail!("Failed to verify a transition output")
            }
            lap!(timer, "Verify the outputs");

            // Retrieve the stack.
            let stack = self.get_stack(transition.program_id())?;
            // Retrieve the function from the stack.
            let function = stack.get_function(transition.function_name())?;

            // Retrieve the caller transition. Note that only the last transition in the execution has no caller.
            let caller = inverted_call_graph.get(transition.id()).and_then(|tid| transition_program_ids.get(tid));

            // Construct the verifier inputs for the transition.
            let inputs = self.to_transition_verifier_inputs(transition, caller, &call_graph, &mut transition_map)?;
            lap!(timer, "Constructed the verifier inputs for a transition of {}", function.name());

            // Save the verifying key and its inputs.
            verifier_inputs
                .entry(Locator::new(*stack.program_id(), *function.name()))
                // Retrieve the verifying key, if it does not already exist.
                .or_insert((stack.get_verifying_key(function.name())?, vec![]))
                .1
                .push(inputs);
            lap!(timer, "Stored the verifier inputs for a transition of {}", function.name());

            // Add the transition to the transition map.
            transition_map.insert(*transition.id(), transition);
        }

        // Count the number of verifier instances.
        let num_instances = verifier_inputs.values().map(|(_, inputs)| inputs.len()).sum::<usize>();
        // Ensure the number of instances matches the number of transitions.
        ensure!(num_instances == execution.transitions().len(), "The number of verifier instances is incorrect");

        // Construct the list of verifier inputs.
        let verifier_inputs: Vec<_> = verifier_inputs.values().cloned().collect();
        // Verify the execution proof.
        Trace::verify_execution_proof(&locator, verifier_inputs, execution)?;

        lap!(timer, "Verify the proof");

        finish!(timer);
        Ok(())
    }
}

impl<N: Network> Process<N> {
    /// Returns the public inputs to verify the proof for the given transition.
    fn to_transition_verifier_inputs(
        &self,
        transition: &Transition<N>,
        caller: Option<&ProgramID<N>>,
        call_graph: &HashMap<N::TransitionID, Vec<N::TransitionID>>,
        transition_map: &mut HashMap<N::TransitionID, &Transition<N>>,
    ) -> Result<Vec<N::Field>> {
        // Compute the x- and y-coordinate of `tpk`.
        let (tpk_x, tpk_y) = transition.tpk().to_xy_coordinates();

        // Compute `is_root` and `caller`.
        let (is_root, caller) = match caller {
            Some(program_id) => (Field::<N>::zero(), *program_id),
            None => (Field::one(), *transition.program_id()),
        };
        // Compute the x- and y-coordinate of `caller`.
        let (caller_x, caller_y) = caller.to_address()?.to_xy_coordinates();

        // [Inputs] Construct the verifier inputs to verify the proof.
        let mut inputs = vec![N::Field::one(), *tpk_x, *tpk_y, **transition.tcm()];
        // [Inputs] Extend the verifier inputs with the input IDs.
        inputs.extend(transition.inputs().iter().flat_map(|input| input.verifier_inputs()));
        // [Inputs] Extend the verifier inputs with the 'self.caller' public inputs.
        inputs.extend([*is_root, *caller_x, *caller_y]);

        // If there are function calls, append their inputs and outputs.
        for transition_id in call_graph.get(transition.id()).unwrap() {
            // Note that this unwrap is safe, since we are processing transitions in post-order, which implies that all callees have been added to `transition_map`.
            let transition: &&Transition<N> = transition_map.get(transition_id).unwrap();

            // [Inputs] Extend the verifier inputs with the input IDs of the external call.
            inputs.extend(transition.inputs().iter().flat_map(|input| input.verifier_inputs()));
            // [Inputs] Extend the verifier inputs with the output IDs of the external call.
            inputs.extend(transition.output_ids().map(|id| **id));
        }

        // [Inputs] Extend the verifier inputs with the output IDs.
        inputs.extend(transition.outputs().iter().flat_map(|output| output.verifier_inputs()));

        #[cfg(debug_assertions)]
        println!("Transition public inputs ({} elements): {:#?}", inputs.len(), inputs);
        Ok(inputs)
    }
}

impl<N: Network> Process<N> {
    // A helper function to construct a call graph from an execution.
    // The call graph is represented as a mapping from the transition ID of a caller transition to the transition IDs of its callees, in the order in which they were called.
    //
    // Suppose we have the following call structure.
    // The functions are invoked in the following order:
    // "three.aleo/a"
    //   --> "two.aleo/b"
    //        --> "zero.aleo/c"
    //   --> "zero.aleo/c"
    //   --> "one.aleo/d"
    //        --> "zero.aleo/c"
    // The order of the transitions in the `Execution` is:
    //  - [c, b, c, c, d, a]
    // However, the `Execution` only provides `Transition`s and not the call graph.
    // In other words, we do not know which transitions were invoked by which transitions.
    // Note that transition names are insufficient to reconstruct the call graph, since the same function can be invoked multiple times, in different ways.
    //
    // In order to reconstruct the call graph, we:
    // - Iterate over the call structure in reverse post-order. The ordering is maintained by the `traversal_stack`.
    // - Process each transition in the `Execution` in reverse, assigning its transition ID to the corresponding function call.
    pub fn construct_call_graph(
        &self,
        execution: &Execution<N>,
    ) -> Result<HashMap<N::TransitionID, Vec<N::TransitionID>>> {
        // Metadata for each transition the execution.
        struct TransitionMetadata<N: Network> {
            uid: usize,
            pid: ProgramID<N>,
            fname: Identifier<N>,
            tid: Option<N::TransitionID>,
            callees: Option<Vec<usize>>,
        }

        impl<N: Network> TransitionMetadata<N> {
            fn new(counter: &mut usize, pid: ProgramID<N>, fname: Identifier<N>, tid: Option<N::TransitionID>) -> Self {
                let uid = *counter;
                *counter += 1;
                Self { uid, pid, fname, tid, callees: None }
            }

            /// Returns 'true' if the subgraph starting from this transition has been fully-indexed.
            fn is_complete(&self) -> bool {
                self.tid.is_some() && self.callees.is_some()
            }
        }

        // A helper function to update the call graph, given transition metadata.
        let update_call_graph = |metadata: TransitionMetadata<N>,
                                 call_graph: &mut HashMap<N::TransitionID, Vec<N::TransitionID>>,
                                 uid_to_tid: &mut HashMap<usize, N::TransitionID>|
         -> Result<()> {
            // Check that the transition metadata is complete.
            ensure!(metadata.is_complete(), "Invalid traversal - transition metadata is incomplete");
            // Update the call graph.
            call_graph.insert(
                metadata.tid.unwrap(),
                metadata
                    .callees // Safe to unwrap, since the metadata is complete.
                    .unwrap()
                    .into_iter()
                    .map(|uid| match uid_to_tid.get(&uid) {
                        Some(tid) => Ok(*tid),
                        None => bail!("Invalid traversal - missing 'tid' for uid '{uid}'"),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
            // Update the UID to TID mapping.
            uid_to_tid.insert(metadata.uid, metadata.tid.unwrap());
            Ok(())
        };

        // Initialize a call graph, which is a map of transition IDs to the transition IDs it calls.
        let mut call_graph = HashMap::new();
        // Initialize a mapping from UIDs to transition IDs.
        let mut uid_to_tid = HashMap::new();

        // Initialize a stack to track transition metadata, while traversing the call graph.
        let mut traversal_stack: Vec<TransitionMetadata<N>> = Vec::new();
        // Initialize a counter to provide unique IDs for each transition.
        let mut counter = 0;

        // Iterate over each transition in reverse post-order, and populate the call graph.
        for transition in execution.transitions().rev() {
            // Now process the current `transition`.
            // At this point, the algorithm must maintain the following invariant:
            // - The stack is either empty, or the top entry is incomplete.
            match traversal_stack.last_mut() {
                // If the stack is empty, then push the `transition` to the top of the stack.
                None => {
                    traversal_stack.push(TransitionMetadata::new(
                        &mut counter,
                        *transition.program_id(),
                        *transition.function_name(),
                        Some(*transition.id()),
                    ));
                }
                // If the stack is not empty, then add the current transition ID to the entry.
                Some(head) => match head.pid == *transition.program_id() && head.fname == *transition.function_name() {
                    true => head.tid = Some(*transition.id()),
                    false => bail!("Invalid traversal - unexpected transition in the execution"),
                },
            }

            // Process the entry at the top of the stack. By the previous step, this entry has a transition ID.
            // Note this unwrap is safe, since we either pushed an entry to the stack or modified the one at the top of the stack.
            let top = traversal_stack.last().unwrap();
            // If the entry is complete, then add it to the call graph.
            if top.is_complete() {
                // Note this unwrap is safe, for the same reason as above.
                update_call_graph(traversal_stack.pop().unwrap(), &mut call_graph, &mut uid_to_tid)?;
            } else {
                // Retrieve the stack.
                let stack = self.get_stack(top.pid)?;
                // Retrieve the function from the stack.
                let function = stack.get_function(&top.fname)?;
                // Collect the callees of the current transition.
                let mut callees = Vec::new();
                for instruction in function.instructions() {
                    if let Instruction::Call(call) = instruction {
                        let (pid, fname) = match call.operator() {
                            synthesizer_program::CallOperator::Locator(locator) => {
                                (locator.program_id(), locator.resource())
                            }
                            synthesizer_program::CallOperator::Resource(fname) => (&top.pid, fname),
                        };
                        // Add the callee to the traversal stack, only if it is a call to a transition.
                        if self.get_stack(pid)?.get_function(fname).is_ok() {
                            callees.push(TransitionMetadata::new(&mut counter, *pid, *fname, None));
                        }
                    }
                }

                // Add the callees UIDs to the metadata.
                // Note this unwrap is safe, for the same reason as above.
                let top = traversal_stack.last_mut().unwrap();
                let callee_uids = callees.iter().map(|callee| callee.uid).collect::<Vec<_>>();
                match top.callees {
                    None => top.callees = Some(callee_uids),
                    Some(_) => bail!("Invalid traversal - callees have already been processed"),
                }
                // Push the callees to the top of the stack.
                traversal_stack.extend(callees);
            }
            // If the stack has complete metadata entries, then remove and add them to the call graph.
            while let Some(metadata) = traversal_stack.last() {
                if metadata.is_complete() {
                    update_call_graph(traversal_stack.pop().unwrap(), &mut call_graph, &mut uid_to_tid)?;
                } else {
                    break;
                }
            }
        }
        // Check that the the traversal completed correctly.
        ensure!(traversal_stack.is_empty(), "Invalid traversal - traversal stack is not empty");
        ensure!(
            counter == execution.len(),
            "Invalid traversal - counter does not match the number of transitions in the execution"
        );

        Ok(call_graph)
    }

    // A helper function to invert a call graph.
    // The call graph is represented as a mapping from the transition ID of a caller transition to the transition IDs of its callees, in the order in which they were called.
    // The inverted call graph is represented as a mapping from the transition ID of a callee transition to the transition ID of its caller.
    // Note that each callee transition can only have one caller transition.
    fn invert_call_graph(
        call_graph: &HashMap<N::TransitionID, Vec<N::TransitionID>>,
    ) -> HashMap<N::TransitionID, N::TransitionID> {
        let mut inverted_call_graph = HashMap::new();
        for (caller, callees) in call_graph {
            for callee in callees {
                let result = inverted_call_graph.insert(*callee, *caller);
                debug_assert!(result.is_none(), "The call graph is not a DAG");
            }
        }
        inverted_call_graph
    }
}
