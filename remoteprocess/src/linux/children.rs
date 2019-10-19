// This code is adapted from rbspy:
// https://github.com/rbspy/rbspy/blob/3d09e3ced011eb10ab7a0f5906659820043e177e/src/ui/descendents.rs
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

use std::collections::HashMap;
use std::fs::read_dir;
use std::fs::File;
use std::io::Read;

use super::{ Pid, Error };

pub fn children(ppid: Pid) -> Result<Vec<Pid>, Error> {
    let parents_to_children = map_parents_to_children()?;
    get_children(ppid, parents_to_children)
}

fn get_children(
    parent_pid: Pid,
    parents_to_children: HashMap<Pid, Vec<Pid>>,
) -> Result<Vec<Pid>, Error> {
    let mut result = Vec::<Pid>::new();
    let mut queue = Vec::<Pid>::new();
    queue.push(parent_pid);

    loop {
        match queue.pop() {
            None => {
                return Ok(result);
            }
            Some(current_pid) => {
                if let Some(children) = parents_to_children.get(&current_pid) {
                    for child in children {
                        queue.push(*child);
                    }
                }
                result.push(current_pid);
            }
        }
    }
}

fn map_parents_to_children() -> Result<HashMap<Pid, Vec<Pid>>, Error> {
    let mut pid_map: HashMap<Pid, Vec<Pid>> = HashMap::new();

    for (pid, ppid) in get_proc_children()? {
        pid_map.entry(ppid).or_insert_with(|| vec![]).push(pid);
    }
    Ok(pid_map)
}

fn status_file_ppid(status: &str) -> Result<Pid, Error> {
    let err = Error::Other(
        format!("Failed to parse process status file {}", status)
    );

    status.split('\n')
        .find(|x| x.starts_with("PPid:"))
        .and_then(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            parts[1].parse::<Pid>().ok()
        })
        .ok_or(err)
}

#[cfg(target_os = "linux")]
fn get_proc_children() -> Result<Vec<(Pid, Pid)>, Error> {
    let mut process_pairs = vec![];
    for entry in read_dir("/proc")? {
        let entry = entry?;
        // try parsing the directory name as a PID and see if it works
        let maybe_pid = entry.file_name().to_string_lossy().parse::<Pid>();
        if let Ok(pid) = maybe_pid {
            let mut contents = String::new();
            if let Ok(mut f) = File::open(entry.path().join("status")) {
                f.read_to_string(&mut contents)?;
                let ppid = status_file_ppid(&contents)?;
                process_pairs.push((pid, ppid));
            }
        }
    }
    Ok(process_pairs)
}

#[test]
fn test_get_children_depth_2() {
    let mut map = HashMap::new();
    map.insert(1, vec![2, 3]);
    map.insert(2, vec![4]);
    let desc = get_children(1, map).unwrap();
    assert_eq!(desc, vec![1, 3, 2, 4]);
}

#[test]
fn test_status_file_ppid() {
    let status = "Name:	kthreadd\nState:	S (sleeping)\nTgid:	2\nNgid:	0\nPid:	0\nPPid:	1234\n";
    assert_eq!(status_file_ppid(status).unwrap(), 1234)
}
