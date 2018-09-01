// This code is taken from the flamegraph.rs from rbspy
// https://github.com/rbspy/rbspy/tree/master/src/ui/flamegraph.rs
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

use std;
use std::collections::HashMap;
use std::io::Write;
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

use failure::{Error, ResultExt};
use tempdir;

use stack_trace::StackTrace;

const FLAMEGRAPH_SCRIPT: &[u8] = include_bytes!("../vendor/flamegraph/flamegraph.pl");

pub struct Flamegraph {
    pub counts: HashMap<Vec<u8>, usize>,
    pub show_linenumbers: bool,
}

impl Flamegraph {
    pub fn new(show_linenumbers: bool) -> Flamegraph {
        Flamegraph { counts: HashMap::new(), show_linenumbers }
    }

    pub fn increment(&mut self, traces: &[StackTrace]) -> std::io::Result<()> {
        for trace in traces {
            if !(trace.active) {
                continue;
            }
            let mut buf = vec![];
            for frame in trace.frames.iter().rev() {
                let filename = match &frame.short_filename { Some(f) => &f, None => &frame.filename };
                if self.show_linenumbers {
                    write!(&mut buf, "{} ({}:{});", frame.name, filename, frame.line)?;
                } else {
                    write!(&mut buf, "{} ({});", frame.name, filename)?;
                }
            }
            *self.counts.entry(buf).or_insert(0) += 1;
        }
        Ok(())
    }

    pub fn write(&self, w: File) -> Result<(), Error> {
        let tempdir = tempdir::TempDir::new("flamegraph").unwrap();
        let stacks_file = tempdir.path().join("stacks.txt");
        let mut file = File::create(&stacks_file).expect("couldn't create file");
        for (k, v) in &self.counts {
            file.write_all(&k)?;
            writeln!(file, " {}", v)?;
        }
        write_flamegraph(&stacks_file, w)
    }
}

fn write_flamegraph(source: &Path, target: File) -> Result<(), Error> {
    let mut child = Command::new("perl")
        .arg("-")
        .arg("--inverted") // icicle graphs are easier to read
        .arg("--minwidth").arg("2") // min width 2 pixels saves on disk space
        .arg(source)
        .stdin(Stdio::piped()) // pipe in the flamegraph.pl script to stdin
        .stdout(target)
        .spawn()
        .context("Couldn't execute perl")?;
    // TODO(nll): Remove this silliness after non-lexical lifetimes land.
    {
        let stdin = child.stdin.as_mut().expect("failed to write to stdin");
        stdin.write_all(FLAMEGRAPH_SCRIPT)?;
    }
    child.wait()?;
    Ok(())
}
