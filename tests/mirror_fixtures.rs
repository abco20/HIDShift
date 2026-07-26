#![cfg(feature = "dual-s3-wired")]

use hidshift::mirror::{MirrorRejectReason, validate_mirror_image};

const PROFILE_A: &[u8] = include_bytes!("../e2e/fixtures/mirror/composite-a.hsmi");
const PROFILE_B: &[u8] = include_bytes!("../e2e/fixtures/mirror/mouse-b.hsmi");
const INVALID_DUPLICATE: &[u8] =
    include_bytes!("../e2e/fixtures/mirror/invalid-duplicate-endpoint.hsmi");

#[test]
fn generated_profiles_preserve_distinct_identity_and_endpoint_addresses() {
    let a = validate_mirror_image(PROFILE_A).unwrap();
    let b = validate_mirror_image(PROFILE_B).unwrap();
    assert_eq!(
        u16::from_le_bytes([a.device_descriptor[10], a.device_descriptor[11]]),
        0x4001
    );
    assert_eq!(
        u16::from_le_bytes([b.device_descriptor[10], b.device_descriptor[11]]),
        0x4002
    );
    assert_eq!(
        a.endpoints
            .iter()
            .map(|endpoint| endpoint.address)
            .collect::<Vec<_>>(),
        [0x81, 0x01, 0x82, 0x02]
    );
    assert_eq!(
        b.endpoints
            .iter()
            .map(|endpoint| endpoint.address)
            .collect::<Vec<_>>(),
        [0x83]
    );
}

#[test]
fn invalid_fixture_is_rejected_for_the_expected_reason() {
    assert_eq!(
        validate_mirror_image(INVALID_DUPLICATE),
        Err(MirrorRejectReason::DuplicateEndpointAddress)
    );
}
