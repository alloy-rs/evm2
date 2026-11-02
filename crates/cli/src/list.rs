use crate::{
    args::List,
    error::{Error, Result},
    fixture,
};
use evm2_eest::FixtureKind;

pub(crate) fn run(command: List) -> Result<()> {
    if FixtureKind::from_path(&command.path).is_some() {
        let suite = fixture::read_blockchain(&command.path)?;
        let mut names = suite.0.keys().map(String::as_str).collect::<Vec<_>>();
        names.sort_unstable();
        for name in names {
            println!("{name}");
        }
        return Ok(());
    }

    let input = fixture::read_text(&command.path)?;
    if fixture::detect_str(&command.path, &input)?.is_none() {
        return Err(Error::UnknownFixtureKind { path: command.path });
    }
    for name in fixture::test_names(&command.path, &input)? {
        println!("{name}");
    }
    Ok(())
}
