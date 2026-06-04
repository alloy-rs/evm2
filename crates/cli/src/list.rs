use crate::{
    args::List,
    error::{Error, Result},
    fixture,
};

pub(crate) fn run(command: List) -> Result<()> {
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
