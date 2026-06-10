use crate::fuzzer::{
    backend::EvmBackend,
    case::{EvmCase, TARGET},
};
use evm2::interpreter::op;

pub(crate) fn differs(backends: &[&dyn EvmBackend; 2], case: &EvmCase) -> bool {
    backends[0].run(case) != backends[1].run(case)
}

pub(crate) fn minimize_case(backends: &[&dyn EvmBackend; 2], mut case: EvmCase) -> EvmCase {
    minimize_accounts(backends, &mut case);
    minimize_storage(backends, &mut case);
    minimize_calldata(backends, &mut case);
    minimize_target_code(backends, &mut case);
    case
}

fn minimize_accounts(backends: &[&dyn EvmBackend; 2], case: &mut EvmCase) {
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

fn minimize_storage(backends: &[&dyn EvmBackend; 2], case: &mut EvmCase) {
    for account_index in 0..case.accounts.len() {
        while let Some(key) = case.accounts[account_index].storage.keys().next().copied() {
            let mut candidate = case.clone();
            candidate.accounts[account_index].storage.remove(&key);
            if differs(backends, &candidate) {
                *case = candidate;
            } else {
                break;
            }
        }
    }
}

fn minimize_calldata(backends: &[&dyn EvmBackend; 2], case: &mut EvmCase) {
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

fn minimize_target_code(backends: &[&dyn EvmBackend; 2], case: &mut EvmCase) {
    let Some(target_index) = case.accounts.iter().position(|account| account.address == TARGET)
    else {
        return;
    };
    let mut len = case.accounts[target_index].code.len();
    while len > 1 {
        let next = len / 2;
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
