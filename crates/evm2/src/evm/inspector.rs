//! EVM execution inspection hooks.

use crate::{
    EvmTypes,
    interpreter::{Interpreter, Message, MessageResult},
};
use alloy_primitives::{Address, Log, U256};

/// EVM execution inspector.
pub trait Inspector<T: EvmTypes> {
    /// Called after a frame interpreter has been initialized.
    #[inline]
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, T>) {
        let _ = interp;
    }

    /// Called before each instruction executes.
    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, T>) {
        let _ = interp;
    }

    /// Called after each instruction executes.
    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, T>) {
        let _ = interp;
    }

    /// Called when a log is emitted.
    #[inline]
    fn log(&mut self, log: &Log) {
        let _ = log;
    }

    /// Called before a call message executes.
    #[inline]
    fn call(&mut self, message: &mut Message) -> Option<MessageResult> {
        let _ = message;
        None
    }

    /// Called after a call message executes.
    #[inline]
    fn call_end(&mut self, message: &Message, result: &mut MessageResult) {
        let _ = message;
        let _ = result;
    }

    /// Called before a create message executes.
    #[inline]
    fn create(&mut self, message: &mut Message) -> Option<MessageResult> {
        let _ = message;
        None
    }

    /// Called after a create message executes.
    #[inline]
    fn create_end(&mut self, message: &Message, result: &mut MessageResult) {
        let _ = message;
        let _ = result;
    }

    /// Called after a contract self-destructs.
    #[inline]
    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        let _ = contract;
        let _ = target;
        let _ = value;
    }
}
