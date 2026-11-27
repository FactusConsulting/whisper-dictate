use super::*;

#[test]
fn resident_hviske_does_not_need_to_fit_in_free_memory_again() {
    assert_eq!(
        hviske_capacity(
            LocalModel::V6,
            |model| model == LocalModel::V6,
            |_| { panic!("a running service already owns its model memory") }
        ),
        LocalGpuCheck::HviskeServiceReady
    );
}

#[test]
fn stopped_hviske_service_is_not_ready_even_when_the_model_fits() {
    assert_eq!(
        hviske_capacity(LocalModel::V53, |_| false, |_| Some(8192)),
        LocalGpuCheck::HviskeServiceNotReady {
            required_mib: 6144,
            free_mib: Some(8192),
        }
    );
}
