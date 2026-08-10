use crate::{
    args::List,
    error::{Error, Result},
    fixture,
};

pub(crate) fn run(command: List) -> Result<()> {
    if fixture::is_binary_path(&command.path) {
        let suite = fixture::read_blockchain(&command.path)?;
        let mut names = suite.0.keys().map(String::as_str).collect::<Vec<_>>();
        names.sort_unstable();
        for name in names {
            println!("{name}");
        }
        return Ok(());
    }

    let input = fixture::read(&command.path)?;
    if fixture::detect(&input.json).is_none() {
        return Err(Error::UnknownFixtureKind { path: command.path });
    }
    let Some(names) = fixture::test_names(&input.json) else {
        return Err(Error::UnknownFixtureKind { path: command.path });
    };
    for name in names {
        println!("{name}");
    }
    Ok(())
}
