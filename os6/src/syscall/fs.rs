//! File and filesystem-related syscalls

use crate::fs::open_file;
use crate::fs::OSInode;
use crate::fs::OpenFlags;
use crate::fs::Stat;
use crate::fs::StatMode;
use crate::mm::translated_byte_buffer;
use crate::mm::translated_refmut;
use crate::mm::translated_str;
use crate::mm::UserBuffer;
use crate::task::current_task;
use crate::task::current_user_token;
use alloc::sync::Arc;
use core::any::Any;

pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        let file = file.clone();
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        file.write(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        -1
    }
}

pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        let file = file.clone();
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        file.read(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        -1
    }
}

pub fn sys_open(path: *const u8, flags: u32) -> isize {
    let task = current_task().unwrap();
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(inode) = open_file(path.as_str(), OpenFlags::from_bits(flags).unwrap()) {
        let mut inner = task.inner_exclusive_access();
        let fd = inner.alloc_fd();
        inner.fd_table[fd] = Some(inode);
        fd as isize
    } else {
        -1
    }
}

pub fn sys_close(fd: usize) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if inner.fd_table[fd].is_none() {
        return -1;
    }
    inner.fd_table[fd].take();
    0
}

// YOUR JOB: 扩展 easy-fs 和内核以实现以下三个 syscall
pub fn sys_fstat(fd: usize, st: *mut Stat) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd <= 2 {
        // doesn't support std{in, out, err} yet
        return -1;
    }
    let fd_table = &inner.fd_table;
    let Some(Some(file)) = fd_table.get(fd) else { return -1;};
    let Some(inode) = (file as &dyn Any).downcast_ref::<OSInode>() else { return -1;};
    let stat = translated_refmut(token, st);
    *stat = Stat::new(
        inode.ino(),
        if inode.is_file() {
            StatMode::FILE
        } else {
            StatMode::DIR
        },
        todo!(),
    );
    0
}

pub fn sys_linkat(_old_name: *const u8, _new_name: *const u8) -> isize {
    -1
}

pub fn sys_unlinkat(_name: *const u8) -> isize {
    -1
}
