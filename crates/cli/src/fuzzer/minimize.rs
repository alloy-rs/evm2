use crate::fuzzer::{
    backend::EvmBackend,
    case::{EvmCase, TARGET},
};
use evm2::interpreter::op;

pub(crate) fn differs(backends: &mut [&mut dyn EvmBackend; 2], case: &EvmCase) -> bool {
    backends[0].run(case) != backends[1].run(case)
}

pub(crate) fn minimize_case(backends: &mut [&mut dyn EvmBackend; 2], mut case: EvmCase) -> EvmCase {
    minimize_accounts(backends, &mut case);
    minimize_storage(backends, &mut case);
    minimize_calldata(backends, &mut case);
    minimize_target_code(backends, &mut case);
    case
}

fn minimize_accounts(backends: &mut [&mut dyn EvmBackend; 2], case: &mut EvmCase) {
    let mut index = 2;
    while index < case.accounts.len() {
        let mut candidate = case.clone();
        candidate.accounts.remove(index);
        if differs(backends, &candidate) {
            *case = candidate;
        } else {
            index += 1;
        }
    }
}

fn minimize_storage(backends: &mut [&mut dyn EvmBackend; 2], case: &mut EvmCase) {
    for account_index in 0..case.accounts.len() {
        let mut keys = case.accounts[account_index].storage.keys().copied().collect::<Vec<_>>();
        keys.sort_unstable();
        for key in keys {
            let mut candidate = case.clone();
            candidate.accounts[account_index].storage.shift_remove(&key);
            if differs(backends, &candidate) {
                *case = candidate;
            }
        }
    }
}

fn minimize_calldata(backends: &mut [&mut dyn EvmBackend; 2], case: &mut EvmCase) {
    let mut len = case.tx.input.len();
    while len > 0 {
        let next = len / 2;
        let mut candidate = case.clone();
        candidate.tx.input = candidate.tx.input.slice(..next);
        if differs(backends, &candidate) {
            *case = candidate;
            len = next;
        } else {
            break;
        }
    }
}

fn minimize_target_code(backends: &mut [&mut dyn EvmBackend; 2], case: &mut EvmCase) {
    let Some(target_index) = case.accounts.iter().position(|account| account.address == TARGET)
    else {
        return;
    };
    let mut len = case.accounts[target_index].code.len();
    while len > 1 {
        let next = len / 2;
        // The candidate is `next` bytes plus an appended STOP, so it only shrinks the code when
        // `next + 1 < len`. Without this guard a two-byte code (`next == 1`, `next + 1 == len`)
        // would loop forever, re-testing an identical candidate and never making progress.
        if next + 1 >= len {
            break;
        }
        let mut candidate = case.clone();
        let mut code = candidate.accounts[target_index].code.slice(..next).to_vec();
        code.push(op::STOP);
        candidate.accounts[target_index].code = code.into();
        if differs(backends, &candidate) {
            *case = candidate;
            len = next + 1;
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuzzer::{
        case::CaseAccount,
        normalize::{FuzzError, Outcome},
    };
    use alloy_primitives::U256;

    struct SlotBackend {
        mismatch_on_required_slot: bool,
    }

    impl EvmBackend for SlotBackend {
        fn name(&self) -> &'static str {
            "slot"
        }

        fn run(&mut self, case: &EvmCase) -> Outcome {
            if self.mismatch_on_required_slot
                && case.accounts[0].storage.contains_key(&U256::from(1))
            {
                Outcome::error(FuzzError::IntrinsicGasTooLow)
            } else {
                Outcome::error(FuzzError::EmptyTransactionSequence)
            }
        }
    }

    #[test]
    fn storage_minimization_continues_after_required_slot() {
        let mut account = CaseAccount::default();
        account.storage.insert(U256::from(2), U256::from(20));
        account.storage.insert(U256::from(1), U256::from(10));
        let mut case = EvmCase { accounts: vec![account], ..Default::default() };
        let mut baseline = SlotBackend { mismatch_on_required_slot: false };
        let mut conditional = SlotBackend { mismatch_on_required_slot: true };
        let mut backends: [&mut dyn EvmBackend; 2] = [&mut baseline, &mut conditional];

        assert!(differs(&mut backends, &case));
        minimize_storage(&mut backends, &mut case);

        assert_eq!(case.accounts[0].storage.len(), 1);
        assert!(case.accounts[0].storage.contains_key(&U256::from(1)));
        assert!(differs(&mut backends, &case));
    }
}
