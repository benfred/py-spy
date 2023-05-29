// This code is adapted from rbspy:
// https://github.com/rbspy/rbspy/tree/master/src/ui/callgrind.rs
// licensed under the MIT License:
/*
MIT License

Copyright (c) 2016 Julia Evans, Kamal Marhubi
Portions (continuous integration setup) Copyright (c) 2016 Jorge Aparicio

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/

use std::cmp::min;
use std::collections::{BTreeMap, HashMap};
use std::io;

use anyhow::Error;

use crate::stack_trace::Frame;

/*
 * **Notes about the overall design**
 *
 * The Callgrind format encodes performance data basically as a graph, where the nodes are
 * functions (like `a`) and the edges are statistics about calls between functions. The `Locations`
 * struct is where that graph is stored, and to print out the callgrind file at the end we iterate
 * over that struct in a pretty straightforward way.
 *
 * Unlike the flamegraph format (which doesn't care about the order of the stack traces you've
 * collected at all), the callgrind format **does** care about the order. The callgrind format
 * implicitly assumes that we have a tracing profile of our program and that you can get exact
 * counts of the number of calls between every 2 functions. Since rbspy is a sampling profiler,
 * this means we have to make some assumptions to make this format work!
 *
 * **counting function calls**
 *
 * The 'count' field in the `Call` struct attempts to count function calls from a -> b
 * The main assumption we make to do this is that if we have a stack (a,b,c) followed by another
 * one with a common prefix (a,b,f,g), then that represents the same function call `a -> b`. This
 * isn't necessarily true but if we're sampling at a high enough rate it's a reasonable assumption.
 *
 * Here's an example: let's assume we have these 4 stack traces:
 *
 *
 * ```
 * a b c d d g x
 * a b c d d // count calls d -> g and g -> x
 * a b c d   // count calls d -> d
 * a b c d d //
 * a b e f g // count calls b -> c, c -> d, d -> d
 * // end: count calls a -> b, b -> e, e -> f, f -> g
 * ```
 *
 * For the above example, here's the data this callgrind code would store for a, b, c, d in the
 * Locations struct.
 *
 * You can see that there are 3 numbers we track:
 *  * `exclusive`: number of times the function was at the top of a stack trace
 *  * `inclusive`: number of stack traces including a call x -> y
 *  * `count`: number of estimated calls from x -> y during execution (using assumption above)
 *
 * a: {exclusive: 0, calls: {b -> {inclusive: 4, count: 1}}}
 * b: {exclusive: 0, calls: {c -> {inclusive: 3, count: 1}, e -> {inclusive: 1, count: 1}}}
 * c: {exclusive: 0, calls: {d -> {inclusive: 3, count: 1}}}
 * d: {exclusive: 3, calls: {d -> {inclusive: 4, count: 2}, g -> {inclusive: 1, count: 1}}}
 *
 */

// Stats about the relationship between two functions, one of which
// calls the other.
#[derive(Debug)]
struct Call {
    // Estimate of number of times this call was made (see above comment)
    count: usize,

    // Number of stack traces including this call.
    // 'a b c d' includes the call b -> c, 'a b e c d' does not
    inclusive: usize,
}

// Stats about a single function.
#[derive(Debug, Default)]
struct Location {
    // How many times does this function appear at the top of a stack trace
    // where it's the most recent function called?
    exclusive: usize,

    // Data about the calls from this function to other functions.
    calls: HashMap<Frame, Call>,
}

// Stats about all functions found in our samples.
#[derive(Default, Debug)]
struct Locations(HashMap<Frame, Location>);

// Information about a function currently on the stack.
#[derive(Debug)]
struct StackEntry {
    frame: Frame,

    // How many samples were found inside this call only?
    exclusive: usize,

    // How many samples were found in this call, and sub-calls?
    inclusive: usize,
}

// Tracks statistics about a program being sampled.
#[derive(Default, Debug)]
pub struct Stats {
    // The current stack, along with tracking information.
    // The root function is at element zero.
    // Not used in final reporting, only for tracking an ongoing profile.
    stack: Vec<StackEntry>,

    // Overall stats about this program.
    locations: Locations,
}

impl Locations {
    // Get the current stats for a StackFrame. If it's never been seen before,
    // automatically create an empty record and return that.
    fn location(&mut self, frame: &Frame) -> &mut Location {
        if !self.0.contains_key(frame) {
            // Never seen this frame before, insert an empty record.
            let loc = Location {
                ..Default::default()
            };
            self.0.insert(frame.clone(), loc);
        }
        self.0.get_mut(frame).unwrap()
    }

    // Add to our stats the exclusive time for a given function.
    fn add_exclusive(&mut self, entry: &StackEntry) {
        self.location(&entry.frame).exclusive += entry.exclusive;
    }

    // Add to our stats info about a single call from a parent to a child
    // function.
    fn add_inclusive(&mut self, parent: &Frame, child: &StackEntry) {
        let ploc = self.location(parent);
        // If we've never seen this parent-child relationship, insert an empty
        // record.
        let val = ploc.calls.entry(child.frame.clone()).or_insert(Call {
            count: 0,
            inclusive: 0,
        });

        // Add both the count and the inclusive samples count.
        val.count += 1;
        val.inclusive += child.inclusive;
    }
}

impl Stats {
    // Create an empty stats tracker.
    pub fn new() -> Stats {
        Stats {
            ..Default::default()
        }
    }

    // Add a single stack sample to this Stats.
    pub fn add(&mut self, stack: &[Frame]) -> Result<(), Error> {
        // The input sample has the root function at the end. Reverse that!
        let rev: Vec<_> = stack.iter().rev().collect();

        // At this point, the previous stack (self.stack) and the new stack
        // (rev) may have some parts that agree and others that differ:
        //
        // Old stack                      New stack
        // +-------+          ^           +------+
        // | root  |          |           | root |
        // +-------+          |           +------+
        // |   A   |        Common        |   A  |
        // +-------+          |           +------+
        // |   B   |          |           |   B  |
        // +-------+      ^   v    ^      +------+
        // |   C   |      |        |      |   X  |
        // +-------+      |     Only new  +------+
        // |   D   |   Only old    |      |   Y  |
        // +-------+      |        v      +------+
        // |   E   |      |
        // +-------+      v
        //
        // Three sections are important:
        //
        // 1. The common base (root, A,  B)
        // 2. The calls only on the previous stack (C, D, E)
        // 3. The calls only on the new stack (X, Y)

        // 1. Common items we can ignore. Find out how many there are, so we
        // can skip them.
        let mut common = 0;
        let max_common = min(self.stack.len(), rev.len());
        while common < max_common && &self.stack[common].frame == rev[common] {
            common += 1;
        }

        // 2. Items only on the previous stack won't be kept. These already have exclusive +
        //    inclusive counts on them, so we just need to add those exclusive + inclusive counts
        //    into our data. For example if we have:
        //          c -> {exclusive: 2, inclusive: 5}
        //          d -> {exclusive: 0, inclusive: 2}
        //          e -> {exclusive: 5, inclusive: 10}
        //    then we'll:
        //    - add {2, 0, 5} to {c, d, e}'s exclusive counts respectively
        //    - add these counts to calls:
        //         b -> c : {count: 1, inclusive: 17}
        //         c -> d : {count: 1, inclusive: 12}
        //         d -> e : {count: 1, inclusive: 10}
        //    - add 17 to the 'inclusive' count on `b`
        //    We add up the inclusive counts because we only increment the 'inclusive' number on
        //    the top element of the stack, so the 'inclusive' number of every stack element is the
        //    sum of its inclusive property and that of all its children.
        while self.stack.len() > common {
            // For each entry, pop it from our stored stack, and track its
            // exclusive sample count.
            let entry = self.stack.pop().unwrap();
            self.locations.add_exclusive(&entry);

            if let Some(parent) = self.stack.last_mut() {
                // If a parent is present, also track the inclusive sample count.
                self.locations.add_inclusive(&parent.frame, &entry);

                // Inclusive time of the child is also inclusive time of the parent,
                // so attribute it to the parent. If multiple previous items exist,
                // this will in turn be attributed to the grand-parent, etc.
                parent.inclusive += entry.inclusive;
            }
        }
        // Now our stored stack (self.stack) only includes common items, since we
        // popped all the old ones.

        // 3. Add new stack frames to our stored stack.
        for item in rev.iter().skip(common) {
            self.stack.push(StackEntry {
                frame: (*item).clone(),
                exclusive: 0,
                inclusive: 0,
            })
        }
        // Now our stored stack has the same structure as the stack sample (rev).

        // Finally, we have to actually count samples somewhere! Add them to the
        // last entry.
        //
        // We don't increment the inclusive time of everything on the stack here,
        // it's easier to do the addition in step 2 above.
        if let Some(entry) = self.stack.last_mut() {
            entry.exclusive += 1;
            entry.inclusive += 1;
        }
        Ok(())
    }

    // Finish adding samples to this Stats.
    pub fn finish(&mut self) -> Result<(), Error> {
        // To handle whatever remains on the stored stack, we can just add
        // an empty stack. This causes us to integrate info for each of those
        // frames--see step 2 in add().
        self.add(&[])
    }

    // Write a callgrind file based on the stats collected.
    // SEe the format docs here: http://kcachegrind.sourceforge.net/html/CallgrindFormat.html
    pub fn write(&self, w: &mut dyn io::Write) -> Result<(), Error> {
        // Write a header.
        writeln!(w, "# callgrind format")?;
        writeln!(w, "version: 1")?;
        writeln!(w, "creator: py-spy")?;
        writeln!(w, "events: Samples")?;

        // Write the info for each function.
        // Sort first, for consistent ordering.
        let sorted: BTreeMap<_, _> = self.locations.0.iter().collect();
        for (frame, loc) in sorted.iter() {
            writeln!(w)?;
            // Exclusive info, along with filename and function name.
            writeln!(w, "fl={}", frame.filename)?;
            writeln!(w, "fn={}", &frame.name)?;
            writeln!(w, "{} {}", frame.line, loc.exclusive)?;

            // Inclusive info for each function called by this one.
            let csorted: BTreeMap<_, _> = loc.calls.iter().collect();
            for (cframe, call) in csorted.iter() {
                writeln!(w, "cfl={}", cframe.filename)?;
                writeln!(w, "cfn={}", &cframe.name)?;
                writeln!(w, "calls={} {}", call.count, cframe.line)?;
                writeln!(w, "{} {}", frame.line, call.inclusive)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::callgrind::*;

    // Build a test stackframe
    fn f(i: i32) -> Frame {
        Frame {
            name: format!("func{}", i),
            filename: format!("file{}.rs", i),
            short_filename: None,
            line: i,
            locals: None,
            module: None,
        }
    }

    // A stack frame from the same file as another one
    fn fdup() -> Frame {
        Frame {
            name: "funcX".to_owned(),
            filename: "file1.rs".to_owned(),
            short_filename: None,
            line: 42,
            locals: None,
            module: None,
        }
    }

    // Assert that basic stats for a stack frame is as expected.
    fn assert_location(stats: &Stats, f: Frame, exclusive: usize, children: usize) {
        let loc = stats
            .locations
            .0
            .get(&f)
            .expect(format!("No location for {:?}", f).as_ref());
        assert_eq!(loc.exclusive, exclusive, "Bad exclusive time for {:?}", f,);
        assert_eq!(loc.calls.len(), children, "Bad children count for {:?}", f,);
    }

    // Assert that the inclusive stats for a parent/child pair is as expected.
    fn assert_inclusive(
        stats: &Stats,
        parent: Frame,
        child: Frame,
        count: usize,
        inclusive: usize,
    ) {
        let ploc = stats
            .locations
            .0
            .get(&parent)
            .expect(format!("No location for {:?}", parent).as_ref());
        let call = ploc
            .calls
            .get(&child)
            .expect(format!("No call of {:?} in {:?}", child, parent).as_ref());
        assert_eq!(
            call.count, count,
            "Bad inclusive count for {:?} in {:?}",
            child, parent,
        );
        assert_eq!(
            call.inclusive, inclusive,
            "Bad inclusive time for {:?} in {:?}",
            child, parent,
        )
    }

    // Track some fake stats for testing, into a Stats object.
    fn build_test_stats() -> Result<Stats, Error> {
        let mut stats = Stats::new();

        stats.add(&vec![f(1)])?;
        stats.add(&vec![f(3), f(2), f(1)])?;
        stats.add(&vec![f(2), f(1)])?;
        stats.add(&vec![f(3), f(1)])?;
        stats.add(&vec![f(2), f(1)])?;
        stats.add(&vec![f(3), fdup(), f(1)])?;
        stats.finish()?;

        Ok(stats)
    }

    // Test that we aggregate stats correctly.
    #[test]
    fn stats_aggregate() {
        let stats = &build_test_stats().expect("Error build test stats");
        assert!(
            stats.stack.is_empty(),
            "Stack not empty: {:#?}",
            stats.stack
        );
        let len = stats.locations.0.len();
        assert_eq!(len, 4, "Bad location count");
        assert_location(stats, f(1), 1, 3);
        assert_location(stats, f(2), 2, 1);
        assert_location(stats, f(3), 3, 0);
        assert_location(stats, fdup(), 0, 1);
        assert_inclusive(stats, f(1), f(2), 2, 3);
        assert_inclusive(stats, f(1), f(3), 1, 1);
        assert_inclusive(stats, f(1), fdup(), 1, 1);
        assert_inclusive(stats, f(2), f(3), 1, 1);
        assert_inclusive(stats, fdup(), f(3), 1, 1);
    }

    // Test that we can write stats correctly.
    #[test]
    fn stats_write() {
        let expected = "# callgrind format
version: 1
creator: py-spy
events: Samples

fl=file1.rs
fn=func1
1 1
cfl=file2.rs
cfn=func2
calls=2 2
1 3
cfl=file3.rs
cfn=func3
calls=1 3
1 1
cfl=file1.rs
cfn=funcX
calls=1 42
1 1

fl=file2.rs
fn=func2
2 2
cfl=file3.rs
cfn=func3
calls=1 3
2 1

fl=file3.rs
fn=func3
3 3

fl=file1.rs
fn=funcX
42 0
cfl=file3.rs
cfn=func3
calls=1 3
42 1
";

        let mut buf: Vec<u8> = Vec::new();
        build_test_stats()
            .expect("Error building test stats")
            .write(&mut buf)
            .expect("Callgrind write failed");
        let actual = String::from_utf8(buf).expect("Callgrind output not utf8");
        assert_eq!(actual, expected, "Unexpected callgrind output");
    }
}
