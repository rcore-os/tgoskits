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

use axhvc::HyperCallCode;

#[test]
fn hypercall_wire_codes_round_trip_and_reject_unknown_values() {
    let codes = [
        HyperCallCode::HypervisorDisable,
        HyperCallCode::HyperVisorPrepareDisable,
        HyperCallCode::HyperVisorDebug,
        HyperCallCode::HIVCPublishChannel,
        HyperCallCode::HIVCSubscribChannel,
        HyperCallCode::HIVCUnPublishChannel,
        HyperCallCode::HIVCUnSubscribChannel,
        HyperCallCode::HIVCNotify,
    ];

    for (wire, code) in codes.into_iter().enumerate() {
        assert_eq!(HyperCallCode::try_from(wire as u32), Ok(code));
        assert_eq!(code as u32, wire as u32);
    }

    let unknown = HyperCallCode::try_from(999).unwrap_err();
    assert_eq!(unknown.0, 999);
}
