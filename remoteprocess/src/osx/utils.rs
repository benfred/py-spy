use super::*;

extern "C" {
    pub fn thread_suspend(thread: thread_act_t) -> kern_return_t;
    pub fn thread_resume(thread: thread_act_t) -> kern_return_t;
}

pub struct TaskLock {
    task: mach_port_name_t
}

impl TaskLock {
    pub fn new(task: mach_port_name_t) -> Result<TaskLock, std::io::Error> {
        let result = unsafe { mach::task::task_suspend(task) };
        if result != KERN_SUCCESS {
            return Err(std::io::Error::last_os_error());
        }
        Ok(TaskLock{task})
    }
}
impl Drop for TaskLock {
    fn drop (&mut self) {
        let result = unsafe { mach::task::task_resume(self.task) };
        if result != KERN_SUCCESS {
            error!("Failed to resume task {}: {}", self.task, std::io::Error::last_os_error());
        }
    }
}

pub struct ThreadLock {
    thread: thread_act_t
}

impl ThreadLock {
    pub fn new(thread: thread_act_t) -> Result<ThreadLock, std::io::Error> {
        let result = unsafe { thread_suspend(thread) };
        if result != KERN_SUCCESS {
            return Err(std::io::Error::last_os_error());
        }
        Ok(ThreadLock{thread})
    }
}
impl Drop for ThreadLock {
    fn drop (&mut self) {
        let result = unsafe { thread_resume(self.thread) };
        if result != KERN_SUCCESS {
            error!("Failed to resume thread {}: {}", self.thread, std::io::Error::last_os_error());
        }
    }
}
