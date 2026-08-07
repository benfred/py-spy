//! Read-only asyncio task inspection for a remote CPython process.
//!
//! This deliberately does not inject code or call `asyncio.all_tasks()` in the
//! target. Instead it reads asyncio's task registries and the suspended
//! coroutine frames directly from process memory.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Error, Result};
use remoteprocess::ProcessMemory;
use serde_derive::Serialize;

use crate::config::LineNo;
use crate::python_bindings::v3_14_0;
use crate::python_data_access::{
    copy_long, copy_string, copy_type_name, format_variable, DictIterator, SetIterator,
    PY_TPFLAGS_MANAGED_DICT,
};
use crate::python_interpreters::{InterpreterState, ListObject, Object, ThreadState, TypeObject};
use crate::stack_trace::{get_stack_frames, Frame};
use crate::version::Version;

const MAX_TASKS: usize = 100_000;
const MAX_CREATION_FRAMES: usize = 10_000;

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub(crate) struct AsyncioDebugOffsets {
    pub task: AsyncioTaskDebugOffsets,
    pub interpreter: AsyncioInterpreterDebugOffsets,
    pub thread: AsyncioThreadDebugOffsets,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub(crate) struct AsyncioTaskDebugOffsets {
    pub size: u64,
    pub name: u64,
    pub awaited_by: u64,
    pub is_task: u64,
    pub awaited_by_is_set: u64,
    pub coro: u64,
    pub node: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub(crate) struct AsyncioInterpreterDebugOffsets {
    pub size: u64,
    pub tasks_head: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub(crate) struct AsyncioThreadDebugOffsets {
    pub size: u64,
    pub running_loop: u64,
    pub running_task: u64,
    pub tasks_head: u64,
}

impl AsyncioDebugOffsets {
    pub(crate) fn is_sane(&self) -> bool {
        self.task.size >= 128
            && self.task.size < 4096
            && self.task.name < self.task.size
            && self.task.coro < self.task.size
            && self.task.node < self.task.size
            && self.interpreter.tasks_head < self.interpreter.size
            && self.thread.running_task < self.thread.size
            && self.thread.tasks_head < self.thread.size
    }
}

/// A live asyncio task and the Python frame at which its coroutine is suspended.
#[derive(Debug, Clone, Serialize)]
pub struct AsyncioTask {
    /// Address of the Task object in the target process, formatted as hex.
    pub task_id: String,
    /// Task name, when supported by the target Python version.
    pub name: Option<String>,
    /// `running`, `pending`, `cancelled`, or `finished`.
    pub state: String,
    /// The coroutine's current frame, represented like thread-dump frames.
    pub frames: Vec<Frame>,
    /// The stack that created this task, when asyncio debug mode captured it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creation_traceback: Option<Vec<Frame>>,
}

#[derive(Debug, Copy, Clone)]
struct TaskOffsets {
    state: usize,
    coro: usize,
    name: Option<usize>,
    source_traceback: usize,
}

#[derive(Debug, Copy, Clone)]
struct FrameSummaryOffsets {
    filename: usize,
    lineno: usize,
    name: usize,
}

#[derive(Copy, Clone)]
struct InspectionOffsets<'a> {
    python: Option<&'a v3_14_0::_Py_DebugOffsets>,
    asyncio: Option<&'a AsyncioDebugOffsets>,
}

fn task_offsets(version: &Version) -> Option<TaskOffsets> {
    match (version.major, version.minor) {
        (3, 7) => Some(TaskOffsets {
            state: 72,
            coro: 112,
            name: None,
            source_traceback: 64,
        }),
        (3, 8) => Some(TaskOffsets {
            state: 72,
            coro: 112,
            name: Some(120),
            source_traceback: 64,
        }),
        (3, 9) => Some(TaskOffsets {
            state: 80,
            coro: 152,
            name: Some(160),
            source_traceback: 64,
        }),
        (3, 10) => Some(TaskOffsets {
            state: 88,
            coro: 160,
            name: Some(168),
            source_traceback: 72,
        }),
        (3, 11 | 12) => Some(TaskOffsets {
            state: 88,
            coro: 136,
            name: Some(144),
            source_traceback: 72,
        }),
        (3, 13) => Some(TaskOffsets {
            state: 96,
            coro: 120,
            name: Some(128),
            source_traceback: 72,
        }),
        (3, 14) => Some(TaskOffsets {
            state: 104,
            coro: 128,
            name: Some(136),
            source_traceback: 72,
        }),
        _ => None,
    }
}

fn frame_summary_offsets(version: &Version) -> Option<FrameSummaryOffsets> {
    // FrameSummary uses __slots__. CPython lays these slots out in sorted name
    // order immediately after PyObject_HEAD; the slot set changed in 3.11,
    // 3.13, and 3.14.
    match (version.major, version.minor) {
        (3, 7..=10) => Some(FrameSummaryOffsets {
            filename: 24,
            lineno: 32,
            name: 48,
        }),
        (3, 11 | 12) => Some(FrameSummaryOffsets {
            filename: 48,
            lineno: 56,
            name: 72,
        }),
        (3, 13) => Some(FrameSummaryOffsets {
            filename: 56,
            lineno: 64,
            name: 80,
        }),
        (3, 14) => Some(FrameSummaryOffsets {
            filename: 64,
            lineno: 72,
            name: 88,
        }),
        _ => None,
    }
}

fn coroutine_frame_offset(version: &Version) -> Option<usize> {
    match (version.major, version.minor) {
        (3, 7..=10) => Some(16),
        (3, 11) => Some(88),
        (3, 12..=14) => Some(72),
        _ => None,
    }
}

fn module_dict<I, P>(
    interpreter_address: usize,
    process: &P,
    version: &Version,
    wanted: &str,
    debug_offsets: Option<&v3_14_0::_Py_DebugOffsets>,
) -> Result<Option<usize>, Error>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    let modules_ptr_ptr = debug_offsets
        .map(|offsets| {
            (interpreter_address + offsets.interpreter_state.imports_modules as usize)
                as *const *const I::Object
        })
        .unwrap_or_else(|| I::modules_ptr_ptr(interpreter_address));
    let modules: *const I::Object = process
        .copy_pointer(modules_ptr_ptr)
        .context("Failed to copy modules PyObject")?;

    for entry in DictIterator::from(process, version, modules as usize)
        .context("Failed to read sys.modules")?
    {
        let (key, value) = entry?;
        let Ok(module_name) = copy_string(key as *const I::StringObject, process) else {
            continue;
        };
        if module_name != wanted {
            continue;
        }

        let module: I::Object = process.copy_struct(value)?;
        let module_type = process.copy_pointer(module.ob_type())?;
        let dict_offset = module_type.dictoffset();
        if dict_offset <= 0 {
            return Err(format_err!("Module {wanted} has no readable dictionary"));
        }
        let dict: usize = process.copy_struct((value as isize + dict_offset) as usize)?;
        return Ok(Some(dict));
    }
    Ok(None)
}

fn string_keyed_dict<I, P>(
    process: &P,
    version: &Version,
    dict: usize,
) -> Result<HashMap<String, usize>, Error>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    let mut values = HashMap::new();
    for entry in DictIterator::from(process, version, dict)
        .with_context(|| format!("Failed to read string-keyed dict at 0x{dict:x}"))?
    {
        let (key, value) = entry?;
        if let Ok(name) = copy_string(key as *const I::StringObject, process) {
            values.insert(name, value);
        }
    }
    Ok(values)
}

fn object_attributes<I, P>(
    process: &P,
    version: &Version,
    addr: usize,
) -> Result<HashMap<String, usize>, Error>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    let object: I::Object = process.copy_struct(addr)?;
    let object_type = process.copy_pointer(object.ob_type())?;
    let flags = object_type.flags();

    let iterator = if flags & PY_TPFLAGS_MANAGED_DICT != 0 {
        DictIterator::from_managed_dict(process, version, addr, object.ob_type() as usize, flags)?
    } else {
        let dict_offset = object_type.dictoffset();
        if dict_offset <= 0 {
            return Err(format_err!(
                "Object at 0x{addr:x} has no instance dictionary"
            ));
        }
        let dict: usize = process.copy_struct((addr as isize + dict_offset) as usize)?;
        if dict == 0 {
            return Err(format_err!("Object at 0x{addr:x} has an empty dictionary"));
        }
        DictIterator::from(process, version, dict)?
    };

    let mut values = HashMap::new();
    for entry in iterator {
        let (key, value) = entry?;
        if let Ok(name) = copy_string(key as *const I::StringObject, process) {
            values.insert(name, value);
        }
    }
    Ok(values)
}

fn weak_set_members<I, P>(
    process: &P,
    version: &Version,
    weak_set: usize,
) -> Result<Vec<usize>, Error>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    let attributes = object_attributes::<I, P>(process, version, weak_set)
        .with_context(|| format!("Failed to read WeakSet at 0x{weak_set:x}"))?;
    let data = *attributes
        .get("data")
        .ok_or_else(|| format_err!("asyncio WeakSet has no data attribute"))?;
    let mut tasks = Vec::new();
    for entry in SetIterator::from(process, data)? {
        let weakref = entry?;
        // PyWeakReference starts with PyObject_HEAD followed by wr_object.
        let target: usize = process.copy_struct(weakref + 2 * std::mem::size_of::<usize>())?;
        if target != 0 {
            tasks.push(target);
        }
    }
    Ok(tasks)
}

fn set_members<P: ProcessMemory>(process: &P, set: usize) -> Result<Vec<usize>, Error> {
    SetIterator::from(process, set)?.collect()
}

fn registry_tasks<I, P>(
    process: &P,
    version: &Version,
    module: &HashMap<String, usize>,
) -> Result<Vec<usize>, Error>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    let mut tasks = Vec::new();
    if let Some(registry) = module.get("_all_tasks") {
        tasks.extend(
            weak_set_members::<I, P>(process, version, *registry)
                .context("Failed to read asyncio._all_tasks")?,
        );
    }
    if let Some(registry) = module.get("_scheduled_tasks") {
        tasks.extend(
            weak_set_members::<I, P>(process, version, *registry)
                .context("Failed to read asyncio._scheduled_tasks")?,
        );
    }
    if let Some(registry) = module.get("_eager_tasks") {
        tasks.extend(
            set_members(process, *registry).context("Failed to read asyncio._eager_tasks")?,
        );
    }
    Ok(tasks)
}

fn current_tasks<P: ProcessMemory>(
    process: &P,
    version: &Version,
    module: &HashMap<String, usize>,
) -> Result<HashSet<usize>, Error> {
    let mut current = HashSet::new();
    if let Some(tasks) = module.get("_current_tasks") {
        for entry in DictIterator::from(process, version, *tasks)
            .context("Failed to read asyncio._current_tasks")?
        {
            let (_, task) = entry?;
            if task != 0 {
                current.insert(task);
            }
        }
    }
    Ok(current)
}

fn linked_list_tasks<P: ProcessMemory>(
    process: &P,
    head: usize,
    task_node_offset: usize,
) -> Result<Vec<usize>, Error> {
    let root: v3_14_0::llist_node = process.copy_struct(head)?;
    let mut node = root.next as usize;
    let mut seen = HashSet::new();
    let mut tasks = Vec::new();
    while node != 0 && node != head {
        if !seen.insert(node) {
            return Err(format_err!("Cycle in asyncio task list at 0x{node:x}"));
        }
        if tasks.len() >= MAX_TASKS {
            return Err(format_err!(
                "Refusing to read more than {MAX_TASKS} asyncio tasks"
            ));
        }
        tasks.push(
            node.checked_sub(task_node_offset)
                .ok_or_else(|| format_err!("Invalid asyncio task node at 0x{node:x}"))?,
        );
        let entry: v3_14_0::llist_node = process.copy_struct(node)?;
        node = entry.next as usize;
    }
    Ok(tasks)
}

fn python_314_tasks<P: ProcessMemory>(
    interpreter_address: usize,
    process: &P,
    debug_offsets: &v3_14_0::_Py_DebugOffsets,
    asyncio_offsets: &AsyncioDebugOffsets,
) -> Result<(Vec<usize>, HashSet<usize>), Error> {
    let node_offset = asyncio_offsets.task.node as usize;

    let mut tasks = linked_list_tasks(
        process,
        interpreter_address + asyncio_offsets.interpreter.tasks_head as usize,
        node_offset,
    )
    .context("Failed to read interpreter task list")?;
    let mut current = HashSet::new();

    let thread_head_ptr =
        interpreter_address + debug_offsets.interpreter_state.threads_head as usize;
    let mut thread: *mut v3_14_0::PyThreadState = process
        .copy_struct(thread_head_ptr)
        .context("Failed to read Python thread list head")?;
    while !thread.is_null() {
        let thread_addr = thread as usize;
        let task_head = thread_addr + asyncio_offsets.thread.tasks_head as usize;
        tasks.extend(
            linked_list_tasks(process, task_head, node_offset).with_context(|| {
                format!("Failed to read task list for thread 0x{thread_addr:x}")
            })?,
        );

        let running: usize = process
            .copy_struct(thread_addr + asyncio_offsets.thread.running_task as usize)
            .context("Failed to read running asyncio task")?;
        if running != 0 {
            current.insert(running);
        }

        thread = process
            .copy_struct(thread_addr + debug_offsets.thread_state.next as usize)
            .context("Failed to read next Python thread state")?;
    }
    Ok((tasks, current))
}

fn read_task_name<I, P>(process: &P, version: &Version, name: usize) -> Option<String>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    if name == 0 {
        return None;
    }
    match copy_type_name::<I, P>(process, name).ok()?.as_str() {
        "str" => copy_string(name as *const I::StringObject, process).ok(),
        "int" => copy_long(process, version, name)
            .ok()
            .map(|(value, _)| format!("Task-{value}")),
        _ => format_variable::<I, P>(process, version, name, 128).ok(),
    }
}

fn read_creation_traceback<I, P>(
    process: &P,
    version: &Version,
    source_traceback: usize,
) -> Result<Option<Vec<Frame>>, Error>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    if source_traceback == 0 {
        return Ok(None);
    }

    // Pure-Python Task implementations store None when debug capture was off.
    if matches!(
        copy_type_name::<I, P>(process, source_traceback).as_deref(),
        Ok("NoneType")
    ) {
        return Ok(None);
    }

    let offsets = frame_summary_offsets(version)
        .ok_or_else(|| format_err!("Unsupported FrameSummary layout on Python {version}"))?;
    let summary: I::ListObject = process
        .copy_struct(source_traceback)
        .context("Failed to read task creation StackSummary")?;
    let count = summary.size();
    if count > MAX_CREATION_FRAMES {
        return Err(format_err!(
            "Refusing to read more than {MAX_CREATION_FRAMES} task creation frames"
        ));
    }

    let mut frames = Vec::with_capacity(count);
    for index in 0..count {
        let item: usize = process
            .copy_struct(summary.item() as usize + index * std::mem::size_of::<usize>())
            .with_context(|| format!("Failed to read creation frame {index}"))?;
        if item == 0 {
            continue;
        }

        let filename_ptr: usize = process.copy_struct(item + offsets.filename)?;
        let lineno_ptr: usize = process.copy_struct(item + offsets.lineno)?;
        let name_ptr: usize = process.copy_struct(item + offsets.name)?;
        let filename = copy_string(filename_ptr as *const I::StringObject, process)
            .with_context(|| format!("Failed to read filename for creation frame {index}"))?;
        let name = copy_string(name_ptr as *const I::StringObject, process)
            .with_context(|| format!("Failed to read name for creation frame {index}"))?;
        let line = copy_long(process, version, lineno_ptr)
            .with_context(|| format!("Failed to read line number for creation frame {index}"))?
            .0
            .try_into()
            .unwrap_or(0);

        frames.push(Frame {
            name,
            filename,
            module: None,
            short_filename: None,
            line,
            locals: None,
            is_entry: false,
            is_shim_entry: false,
        });
    }
    Ok(Some(frames))
}

fn read_task<I, P>(
    process: &P,
    version: &Version,
    task: usize,
    running: bool,
    copy_locals: bool,
    lineno: LineNo,
    inspection_offsets: InspectionOffsets<'_>,
) -> Result<AsyncioTask, Error>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    let mut offsets = task_offsets(version)
        .ok_or_else(|| format_err!("asyncio task inspection is unsupported on Python {version}"))?;
    if let Some(asyncio_offsets) = inspection_offsets.asyncio {
        offsets.coro = asyncio_offsets.task.coro as usize;
        offsets.name = Some(asyncio_offsets.task.name as usize);
    }

    // Pure-Python Task implementations expose these fields through their
    // instance dictionary. Native _asyncio.Task objects use the C offsets.
    let task_type = copy_type_name::<I, P>(process, task).unwrap_or_default();
    let attributes = if task_type == "_asyncio.Task" {
        None
    } else {
        object_attributes::<I, P>(process, version, task).ok()
    };
    let coro = match attributes.as_ref().and_then(|attrs| attrs.get("_coro")) {
        Some(coro) => *coro,
        None => process.copy_struct(task + offsets.coro)?,
    };
    let name_ptr = attributes
        .as_ref()
        .and_then(|attrs| attrs.get("_name"))
        .copied()
        .or_else(|| {
            offsets
                .name
                .and_then(|offset| process.copy_struct(task + offset).ok())
        });
    let source_traceback_ptr = match attributes
        .as_ref()
        .and_then(|attrs| attrs.get("_source_traceback"))
    {
        Some(source_traceback) => *source_traceback,
        None => process.copy_struct(task + offsets.source_traceback)?,
    };
    let creation_traceback =
        match read_creation_traceback::<I, P>(process, version, source_traceback_ptr) {
            Ok(traceback) => traceback,
            Err(error) => {
                warn!(
                    "Failed to inspect creation traceback for asyncio task at 0x{task:x}: {error}"
                );
                None
            }
        };

    let state = if running {
        "running".to_owned()
    } else if let Some(state) = attributes
        .as_ref()
        .and_then(|attrs| attrs.get("_state"))
        .and_then(|state| copy_string(*state as *const I::StringObject, process).ok())
    {
        state.to_ascii_lowercase()
    } else if version.minor == 14 {
        // Native 3.14 task lists only contain pending tasks. Future state is
        // intentionally not part of CPython's AsyncioDebug table.
        "pending".to_owned()
    } else {
        let state: i32 = process.copy_struct(task + offsets.state)?;
        match state {
            0 => "pending",
            1 => "cancelled",
            2 => "finished",
            _ => "unknown",
        }
        .to_owned()
    };

    let frames = if coro == 0 {
        Vec::new()
    } else {
        let coro_type = copy_type_name::<I, P>(process, coro).unwrap_or_default();
        if matches!(
            coro_type.as_str(),
            "coroutine" | "generator" | "async_generator"
        ) {
            let frame_offset = inspection_offsets
                .python
                .map(|offsets| offsets.gen_object.gi_iframe as usize)
                .or_else(|| coroutine_frame_offset(version))
                .unwrap();
            let frame_addr = if version.minor <= 10 {
                process.copy_struct(coro + frame_offset)?
            } else {
                coro + frame_offset
            };
            let mut frames = get_stack_frames(
                frame_addr as *mut <I::ThreadState as ThreadState>::FrameObject,
                process,
                copy_locals,
                lineno,
            )?;
            // A coroutine owns one current frame. When it is actively running,
            // f_back may lead into the event loop's thread stack; those frames
            // belong to the runner, not to this task.
            frames.truncate(1);
            frames
        } else {
            Vec::new()
        }
    };

    Ok(AsyncioTask {
        task_id: format!("0x{task:x}"),
        name: name_ptr.and_then(|name| read_task_name::<I, P>(process, version, name)),
        state,
        frames,
        creation_traceback,
    })
}

/// Returns every live asyncio task registered in the interpreter.
pub(crate) fn tasks_from_interpreter<I, P>(
    interpreter_address: usize,
    process: &P,
    version: &Version,
    copy_locals: bool,
    lineno: LineNo,
    debug_offsets: Option<&v3_14_0::_Py_DebugOffsets>,
    asyncio_offsets: Option<&AsyncioDebugOffsets>,
) -> Result<Vec<AsyncioTask>, Error>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    if version.major != 3 || !(7..=14).contains(&version.minor) {
        return Err(format_err!(
            "asyncio task inspection requires CPython 3.7 through 3.14 (found {version})"
        ));
    }

    let Some(module_dict) = module_dict::<I, P>(
        interpreter_address,
        process,
        version,
        "asyncio.tasks",
        debug_offsets,
    )
    .context("Failed to locate asyncio.tasks")?
    else {
        return Ok(Vec::new());
    };
    let module = string_keyed_dict::<I, P>(process, version, module_dict)
        .context("Failed to read asyncio.tasks module dictionary")?;
    let mut task_addresses = registry_tasks::<I, P>(process, version, &module)
        .context("Failed to read asyncio task registries")?;
    let mut running =
        current_tasks(process, version, &module).context("Failed to read current asyncio tasks")?;

    if version.minor == 14 {
        let debug_offsets = debug_offsets
            .ok_or_else(|| format_err!("Python 3.14 runtime debug offsets are unavailable"))?;
        let asyncio_offsets = asyncio_offsets
            .ok_or_else(|| format_err!("Python 3.14 asyncio debug offsets are unavailable"))?;
        let (native_tasks, native_running) =
            python_314_tasks(interpreter_address, process, debug_offsets, asyncio_offsets)
                .context("Failed to read Python 3.14 native task lists")?;
        task_addresses.extend(native_tasks);
        running.extend(native_running);
    }

    task_addresses.sort_unstable();
    task_addresses.dedup();
    if task_addresses.len() > MAX_TASKS {
        return Err(format_err!(
            "Refusing to read more than {MAX_TASKS} asyncio tasks"
        ));
    }

    let mut tasks = Vec::with_capacity(task_addresses.len());
    let inspection_offsets = InspectionOffsets {
        python: debug_offsets,
        asyncio: asyncio_offsets,
    };
    for task in task_addresses {
        match read_task::<I, P>(
            process,
            version,
            task,
            running.contains(&task),
            copy_locals,
            lineno,
            inspection_offsets,
        ) {
            Ok(task) => tasks.push(task),
            Err(error) => warn!("Failed to inspect asyncio task at 0x{task:x}: {error}"),
        }
    }
    Ok(tasks)
}
