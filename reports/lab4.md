# 简单总结你实现的功能

把之前的实验中实现的功能迁移过来，实现了 linkat、unlinkat、fstat 三个系统调用。
在实现 unlink 时，我选择将对应的 DirEntry 清空。在 ls 中跳过对应的 DirEntry。
在 create 时优先使用被清空的 DirEntry, 若找不到则追加 DirEntry.

# 问答题

> 在我们的easy-fs中，root inode起着什么作用？

root inode 是唯一的一个目录节点，保存着所有目录项，目录项包含文件名和 inode 编号。

> 如果root inode中的内容损坏了，会发生什么？

可能 root inode 会指向非法的 data block，其中会有非法的 DirEntry. 
这会导致我们读取到垃圾数据，也可能会在我们写入数据时造成对文件系统的更大的破坏。
