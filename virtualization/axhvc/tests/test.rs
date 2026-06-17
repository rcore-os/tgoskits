// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use axhvc::{HyperCallCode, InvalidHyperCallCode};

#[test]
fn test_hypercall_code_from_u32_valid() {
    assert_eq!(
        HyperCallCode::try_from(0u32).unwrap(),
        HyperCallCode::HypervisorDisable
    );
    assert_eq!(
        HyperCallCode::try_from(1u32).unwrap(),
        HyperCallCode::HyperVisorPrepareDisable
    );
    assert_eq!(
        HyperCallCode::try_from(2u32).unwrap(),
        HyperCallCode::HyperVisorDebug
    );
    assert_eq!(
        HyperCallCode::try_from(3u32).unwrap(),
        HyperCallCode::HIVCPublishChannel
    );
    assert_eq!(
        HyperCallCode::try_from(4u32).unwrap(),
        HyperCallCode::HIVCSubscribChannel
    );
    assert_eq!(
        HyperCallCode::try_from(5u32).unwrap(),
        HyperCallCode::HIVCUnPublishChannel
    );
    assert_eq!(
        HyperCallCode::try_from(6u32).unwrap(),
        HyperCallCode::HIVCUnSubscribChannel
    );
}

#[test]
fn test_hypercall_code_from_u32_invalid() {
    assert!(HyperCallCode::try_from(7u32).is_err());
    assert!(HyperCallCode::try_from(100u32).is_err());
    assert!(HyperCallCode::try_from(u32::MAX).is_err());
}

#[test]
fn test_hypercall_code_to_u32() {
    assert_eq!(HyperCallCode::HypervisorDisable as u32, 0);
    assert_eq!(HyperCallCode::HyperVisorPrepareDisable as u32, 1);
    assert_eq!(HyperCallCode::HyperVisorDebug as u32, 2);
    assert_eq!(HyperCallCode::HIVCPublishChannel as u32, 3);
    assert_eq!(HyperCallCode::HIVCSubscribChannel as u32, 4);
    assert_eq!(HyperCallCode::HIVCUnPublishChannel as u32, 5);
    assert_eq!(HyperCallCode::HIVCUnSubscribChannel as u32, 6);
}

#[test]
fn test_hypercall_code_equality() {
    assert_eq!(
        HyperCallCode::HypervisorDisable,
        HyperCallCode::HypervisorDisable
    );
    assert_ne!(
        HyperCallCode::HypervisorDisable,
        HyperCallCode::HyperVisorDebug
    );
}

#[test]
fn test_invalid_hypercall_code_display() {
    let err = InvalidHyperCallCode(0xFF);
    assert_eq!(format!("{}", err), "invalid hypercall code: 0xff");

    let err = InvalidHyperCallCode(7);
    assert_eq!(format!("{}", err), "invalid hypercall code: 0x7");
}

#[test]
fn test_invalid_hypercall_code_debug() {
    let err = InvalidHyperCallCode(42);
    assert_eq!(format!("{:?}", err), "InvalidHyperCallCode(42)");
}

#[test]
fn test_invalid_hypercall_code_from_try_from() {
    let result = HyperCallCode::try_from(999u32);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.0, 999);
}
