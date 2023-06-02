use std::time::{Instant, Duration};
#[cfg(windows)]
use winapi::um::timeapi;

use rand;
use rand_distr::{Exp, Distribution};

/// Timer is an iterator that sleeps an appropriate amount of time between iterations
/// so that we can sample the process a certain number of times a second.
/// We're using an irregular sampling strategy to avoid aliasing effects that can happen
/// if the target process runs code at a similar schedule as the profiler:
/// https://github.com/benfred/py-spy/issues/94
pub struct Timer {
    start: Instant,
    desired: Duration,
    exp: Exp<f64>,
}

impl Timer {
    pub fn new(rate: f64) -> Timer {
        // This changes a system-wide setting on Windows so that the OS wakes up every 1ms
        // instead of the default 15.6ms. This is required to have a sleep call
        // take less than 15ms, which we need since we usually profile at more than 64hz.
        // The downside is that this will increase power usage: good discussions are:
        // https://randomascii.wordpress.com/2013/07/08/windows-timer-resolution-megawatts-wasted/
        // and http://www.belshe.com/2010/06/04/chrome-cranking-up-the-clock/
        #[cfg(windows)]
        unsafe { timeapi::timeBeginPeriod(1); }

        let start = Instant::now();
        Timer{start, desired: Duration::from_secs(0), exp: Exp::new(rate).unwrap()}
    }
}

impl Iterator for Timer {
    type Item = Result<Duration, Duration>;

    fn next(&mut self) -> Option<Self::Item> {
        let elapsed = self.start.elapsed();

        // figure out how many nanoseconds should come between the previous and
        // the next sample using an exponential distribution to avoid aliasing
        let nanos = 1_000_000_000.0 * self.exp.sample(&mut rand::thread_rng());

        // since we want to account for the amount of time the sampling takes
        // we keep track of when we should sleep to (rather than just sleeping
        // the amount of time from the previous line).
        self.desired += Duration::from_nanos(nanos as u64);

        // sleep if appropriate, or warn if we are behind in sampling
        if self.desired > elapsed {
            std::thread::sleep(self.desired - elapsed);
            Some(Ok(self.desired - elapsed))
        } else {
            Some(Err(elapsed - self.desired))
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe { timeapi::timeEndPeriod(1); }
    }
}
