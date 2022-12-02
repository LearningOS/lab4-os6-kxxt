use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use log::{debug, info};
use spin::{Mutex, MutexGuard};

/// Virtual filesystem layer over easy-fs
#[derive(Clone)]
pub struct Inode {
    block_id: usize,
    block_offset: usize,
    fs: Arc<Mutex<EasyFileSystem>>,
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        block_id: u32,
        block_offset: usize,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            block_id: block_id as usize,
            block_offset,
            fs,
            block_device,
        }
    }
    /// Call a function over a disk inode to read it
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// Call a function over a disk inode to modify it
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir());
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if dirent.name() == name {
                return Some(dirent.inode_number() as u32);
            }
        }
        None
    }
    /// Find inode under current inode by name
    pub fn find(&self, name: &str) -> Option<(u64, Arc<Inode>)> {
        let fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            self.find_inode_id(name, disk_inode).map(|inode_id| {
                let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
                (
                    inode_id as u64,
                    Arc::new(Self::new(
                        block_id,
                        block_offset,
                        self.fs.clone(),
                        self.block_device.clone(),
                    )),
                )
            })
        })
    }
    /// Increase the size of a disk inode
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size {
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }

    /// Create hard link
    pub fn hard_link(&self, name: &str, to: &str) -> Option<Arc<Inode>> {
        if self.is_file() {
            return None;
        }
        let Some((to_inode_id, to_inode)) = self.find(to) else { return None; };
        info!("Found target inode");
        let mut fs = self.fs.lock();
        info!("FS Lock Acquired");
        self.modify_disk_inode(|root_inode| {
            info!("Modifying Disk inode!");
            // append file in the dirent
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            // increase size
            self.increase_size(new_size as u32, root_inode, &mut fs);
            // write dirent
            let dirent = DirEntry::new(name, to_inode_id as u32);
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });
        info!("Before modifying target disk inode!");
        to_inode.modify_disk_inode(|inode| inode.nlink += 1);
        block_cache_sync_all();
        Some(to_inode)
    }

    pub fn unlink(&self, name: &str) -> bool {
        let Some((target_id, target)) = self.find(name) else { return false; };
        let mut fs = self.fs.lock();
        target.modify_disk_inode(|inode| {
            inode.nlink -= 1;
            if inode.nlink == 0 {
                // Dealloc data
                let data_blocks = inode.clear_size(&self.block_device);
                for data_block in data_blocks {
                    if data_block > 0 {
                        debug!("Dealloc data block {data_block}");
                        fs.dealloc_data(data_block);
                    }
                }
                // Dealloc inode
                debug!("Dealloc inode {target_id}");
                fs.dealloc_inode(target_id as u32);
                // Remove corresponding dirent
                self.remove_dirent(name);
            }
        });
        block_cache_sync_all();
        true
    }

    fn try_modify_empty_dirent(&self, dirent: &DirEntry) -> bool {
        self.modify_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            for i in 0..file_count {
                let mut empty = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, empty.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                if empty.as_bytes()[0] == 0 {
                    disk_inode.write_at(i * DIRENT_SZ, dirent.as_bytes(), &self.block_device);
                    return true;
                }
            }
            false
        })
    }
    /// Create inode under current inode by name
    pub fn create(&self, name: &str) -> Option<(u64, Arc<Inode>)> {
        let mut fs = self.fs.lock();
        if self
            .modify_disk_inode(|root_inode| {
                // assert it is a directory
                assert!(root_inode.is_dir());
                // has the file been created?
                self.find_inode_id(name, root_inode)
            })
            .is_some()
        {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File);
            });
        // write dirent
        let dirent = DirEntry::new(name, new_inode_id);
        if !self.try_modify_empty_dirent(&dirent) {
            self.modify_disk_inode(|root_inode| {
                // append file in the dirent
                let file_count = (root_inode.size as usize) / DIRENT_SZ;
                let new_size = (file_count + 1) * DIRENT_SZ;
                // increase size
                self.increase_size(new_size as u32, root_inode, &mut fs);

                root_inode.write_at(
                    file_count * DIRENT_SZ,
                    dirent.as_bytes(),
                    &self.block_device,
                );
            });
        }

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some((
            new_inode_id as u64,
            Arc::new(Self::new(
                block_id,
                block_offset,
                self.fs.clone(),
                self.block_device.clone(),
            )),
        ))
        // release efs lock automatically by compiler
    }
    pub fn remove_dirent(&self, name: &str) {
        self.modify_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            (0..file_count).for_each(|i| {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                if dirent.name() == name {
                    disk_inode.write_at(
                        i * DIRENT_SZ,
                        DirEntry::empty().as_bytes(),
                        &self.block_device,
                    );
                }
            })
        })
    }
    /// List direntries with filter applied, only call this fn when you holds a lock
    pub fn dirents_filter(&self, f: impl Fn(&DirEntry) -> bool) -> Vec<DirEntry> {
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            (0..file_count)
                .filter_map(|i| {
                    let mut dirent = DirEntry::empty();
                    assert_eq!(
                        disk_inode.read_at(
                            i * DIRENT_SZ,
                            dirent.as_bytes_mut(),
                            &self.block_device,
                        ),
                        DIRENT_SZ,
                    );
                    // v.push(dirent);
                    if f(&dirent) {
                        Some(dirent)
                    } else {
                        None
                    }
                })
                .collect()
        })
    }
    /// List inodes under current inode
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                if dirent.as_bytes()[0] != 0 {
                    // Cleared dir entry
                    v.push(String::from(dirent.name()));
                }
            }
            v
        })
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Clear the data in current inode
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }

    pub fn is_file(&self) -> bool {
        return self.read_disk_inode(|x| x.is_file());
    }

    pub fn nlink(&self) -> u32 {
        return self.read_disk_inode(|x| x.nlink);
    }
}
