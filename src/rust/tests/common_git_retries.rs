mod common;

use common::with_git_retries;
use std::io::Error;

#[test]
fn returns_the_first_success_and_stops_retrying() {
    let mut calls = 0;
    let value = with_git_retries(3, || -> Result<u32, Error> {
        calls += 1;
        if calls < 2 {
            Err(Error::other("transient"))
        } else {
            Ok(calls)
        }
    })
    .expect("second attempt succeeds");
    assert_eq!(value, 2);
    assert_eq!(calls, 2, "a success must stop the retry loop");
}

#[test]
fn reports_the_last_error_after_exhausting_attempts() {
    let mut calls = 0;
    let err = with_git_retries(3, || -> Result<u32, Error> {
        calls += 1;
        Err(Error::other(format!("fail {calls}")))
    })
    .expect_err("every attempt fails");
    assert_eq!(err, "fail 3", "the last error is reported");
    assert_eq!(calls, 3, "exactly `attempts` calls are made");
}

#[test]
fn a_first_try_success_makes_zero_retries() {
    let mut calls = 0;
    let value = with_git_retries(4, || -> Result<&'static str, Error> {
        calls += 1;
        Ok("immediate")
    })
    .expect("first attempt succeeds");
    assert_eq!(value, "immediate");
    assert_eq!(calls, 1);
}
