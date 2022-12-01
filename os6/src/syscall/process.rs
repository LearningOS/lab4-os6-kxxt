//! Process management syscalls

use crate::config::MAX_SYSCALL_NUM;
use crate::fs::{open_file, OpenFlags};
use crate::mm::{translated_ref, translated_refmut, translated_str, MapPermission, VirtAddr};
use crate::task::{
    add_task, current_task, current_user_token, exit_current_and_run_next,
    suspend_current_and_run_next, TaskStatus, PROCESSOR,
};
use crate::timer::get_time_us;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

#[derive(Clone, Copy)]
pub struct TaskInfo {
    pub status: TaskStatus,
    pub syscall_times: [u32; MAX_SYSCALL_NUM],
    pub time: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    debug!("[kernel] Application exited with code {}", exit_code);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    current_task().unwrap().pid.0 as isize
}

/// Syscall Fork which returns 0 for child process and child_pid for parent process
pub fn sys_fork() -> isize {
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

/// Syscall Exec which accepts the elf path
pub fn sys_exec(path: *const u8) -> isize {
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        task.exec(all_data.as_slice());
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    let task = current_task().unwrap();
    // find a child process

    // ---- access current TCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB lock exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after removing from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child TCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB lock automatically
}

pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    let us = get_time_us();
    let v_addr: VirtAddr = (ts as usize).into();
    PROCESSOR.exclusive_access().set_val_in_current_task(
        v_addr,
        TimeVal {
            sec: us / 1_000_000,
            usec: us % 1_000_000,
        },
    );
    0
}

pub fn sys_task_info(ti: *mut TaskInfo) -> isize {
    let v_addr: VirtAddr = (ti as usize).into();
    let pro = PROCESSOR.exclusive_access();
    let current = pro.current();
    let tcb_inner = current.as_ref().unwrap().inner_exclusive_access();
    let syscall_times = tcb_inner.syscall_times;
    let start_time = tcb_inner.start_time;
    // drop(tcb_inner);
    pro.set_val_in_current_task(
        v_addr,
        TaskInfo {
            status: TaskStatus::Running,
            syscall_times,
            time: get_time_us() - start_time,
        },
    );
    0
}

pub fn sys_set_priority(prio: isize) -> isize {
    if prio < 2 {
        return -1;
    }
    let current = current_task();
    let mut inner = current.as_ref().unwrap().inner_exclusive_access();
    inner.set_priority(prio as usize);
    prio
}

bitflags! {
    struct MmapPort : usize {
        const R = 0b001;
        const W = 0b010;
        const X = 0b100;
    }
}

// YOUR JOB: 扩展内核以实现 sys_mmap 和 sys_munmap
pub fn sys_mmap(start: usize, len: usize, port: usize) -> isize {
    let Some(flags) = MmapPort::from_bits(port) else { return -1;};
    let s_addr = VirtAddr::from(start);
    let offset = s_addr.page_offset();
    if port & 0b111 == 0 || offset != 0 {
        // 1. Meaningless combination
        // 2. start not aligned by page size

        debug!("MMAP got invalid argument!, port = {port}, s_addr = {s_addr:?}, offset = {offset}");
        return -1;
    }
    if len == 0 {
        // No allocation at all.
        // TODO: check
        return 0;
    }
    let flags = MapPermission::U | MapPermission::from_bits((flags.bits as u8) << 1).unwrap();
    trace!("PTEFlags: {flags:?}");
    let task = current_task().unwrap();
    let mut tcb_inner = task.inner_exclusive_access();
    if tcb_inner
        .memory_set
        .mmap(s_addr, VirtAddr::from(start + len), flags)
    {
        0
    } else {
        -1
    }
}

pub fn sys_munmap(start: usize, len: usize) -> isize {
    let s_addr = VirtAddr::from(start);
    if s_addr.page_offset() != 0 {
        // start not aligned by page size
        return -1;
    }
    if len == 0 {
        // no need to unmap
        return 0;
    }
    let task = current_task().unwrap();
    let mut tcb_inner = task.inner_exclusive_access();
    if tcb_inner
        .memory_set
        .munmap(s_addr, VirtAddr::from(start + len))
    {
        0
    } else {
        -1
    }
}

//
// YOUR JOB: 实现 sys_spawn 系统调用
// ALERT: 注意在实现 SPAWN 时不需要复制父进程地址空间，SPAWN != FORK + EXEC
pub fn sys_spawn(path: *const u8) -> isize {
    let token = current_user_token();
    let path = translated_str(token, path);
    let Some(file) = open_file(path.as_str(), OpenFlags::RDONLY) else { return -1;};
    let elf_data = file.read_all();
    let Some(tcb) = current_task().unwrap().spawn(&elf_data) else { return -1; };
    return tcb.getpid() as isize;
}
