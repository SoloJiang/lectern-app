use crate::profile::Profile;

/// The per-profile graph test kernel.md asks `app` for: a wrong graph
/// fails CI rather than a release.
#[test]
fn every_profile_graph_validates() {
    for profile in Profile::ALL {
        if let Err(errors) = profile.plugins().validate() {
            panic!("{profile:?}: {errors:?}");
        }
    }
}
