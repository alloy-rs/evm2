use crate::{
    args::Replay,
    error::{Error, Result},
    fixture::{self, FixtureKind},
};
use evm2_eest::{
    BlockchainTestExecuteConfig, EntryPoint, StateTestExecuteConfig,
    execute_blockchain_tests_str_with_filter, execute_state_tests_str_with_filter,
};

pub(crate) fn run(command: Replay) -> Result<()> {
    let input = fixture::read(&command.path)?;
    let entrypoint = EntryPoint::new(command.entrypoint);
    match fixture::detect(&input.json) {
        Some(FixtureKind::StateTest) => {
            let summary = execute_state_tests_str_with_filter(
                &command.path,
                &input.text,
                StateTestExecuteConfig::default(),
                &entrypoint,
            )
            .map_err(|source| Error::StateTest { source })?;
            println!(
                "replayed state fixture {}: {} executed, {} skipped",
                command.path.display(),
                summary.executed,
                summary.skipped
            );
            Ok(())
        }
        Some(FixtureKind::BlockchainTest) => {
            let summary = execute_blockchain_tests_str_with_filter(
                &command.path,
                &input.text,
                BlockchainTestExecuteConfig::default(),
                &entrypoint,
            )
            .map_err(|source| Error::BlockchainTest { source })?;
            println!(
                "replayed blockchain fixture {}: {} executed, {} skipped",
                command.path.display(),
                summary.executed,
                summary.skipped
            );
            Ok(())
        }
        None => Err(Error::UnknownFixtureKind { path: command.path }),
    }
}
