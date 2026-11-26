//! Identity-checked explicit cleanup outside all operation gates.

use super::*;

impl Binding {
    pub(super) fn cleanup(
        &self,
        target: &Location,
        mounter: &MutationCredentials<'_>,
    ) -> VfsResult<NamespaceOutcome<NamespaceEffect>> {
        let expected = identity(target);
        let outcome = NamespaceCommandScope::run_write(&self.write, |command| {
            loop {
                let scope = command.attempt()?;
                let mut mismatched = false;
                {
                    let context = scope.context();
                    match scope.unlink(
                        &context,
                        self.work.prepare_unlink(&self.name)?,
                        target.is_dir(),
                        mounter,
                    )? {
                        NamespaceStart::Observed(observed) => {
                            if expected.matches(&observed.target().entry) {
                                observed.resume();
                            } else {
                                observed.cancel();
                                mismatched = true;
                            }
                        }
                        NamespaceStart::Complete(_) => {}
                    }
                }
                let outcome = scope
                    .finish()
                    .unwrap_or_else(|_| unreachable!("bound stage cleanup"));
                if mismatched {
                    return Ok(NamespaceOutcome::unchanged(
                        outcome.result().and(Err(VfsError::BadState)),
                    ));
                }
                if outcome.state() != NamespaceState::Retry {
                    return Ok(outcome);
                }
                command.prepare()?;
            }
        })?;
        if outcome.result() == Err(VfsError::NotFound)
            && outcome.state() == NamespaceState::Unchanged
        {
            Ok(NamespaceOutcome::unchanged(Ok(())))
        } else {
            Ok(outcome)
        }
    }
}
