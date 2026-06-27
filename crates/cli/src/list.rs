use crate::{
    args::List,
    error::{Error, Result},
    fixture,
};

pub(crate) fn run(command: List) -> Result<()> {
    if fixture::is_binary_path(&command.path) {
        let suite = fixture::read_blockchain(&command.path)?;
        let mut entrypoints = suite.0.keys().map(String::as_str).collect::<Vec<_>>();
        entrypoints.sort_unstable();
        for entrypoint in entrypoints {
            println!("{entrypoint}");
        }
        return Ok(());
    }

    let input = fixture::read(&command.path)?;
    if fixture::detect(&input.json).is_none() {
        return Err(Error::UnknownFixtureKind { path: command.path });
    }
    let Some(entrypoints) = fixture::entrypoints(&input.json) else {
        return Err(Error::UnknownFixtureKind { path: command.path });
    };
    for entrypoint in entrypoints {
        println!("{entrypoint}");
    }
    Ok(())
}
