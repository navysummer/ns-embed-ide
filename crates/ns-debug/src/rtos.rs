//! Real FreeRTOS kernel task enumeration for the RTOS view.
//!
//! The FreeRTOS-style template exposed by the IDE uses plain globals
//! (`task_count`, `current_task`, `tasks[i].field`). A *real* FreeRTOS kernel
//! keeps tasks in a set of intrusive scheduler lists:
//!
//! - `xReadyTasksLists[configMAX_PRIORITIES]` — one list per priority
//! - `xDelayedTaskList1` / `xDelayedTaskList2` — sleeping / delayed tasks
//! - `xPendingReadyList` — tasks moved to ready but not yet scheduled
//! - `xSuspendedTaskList` — suspended tasks
//! - `xTasksWaitingTermination` — tasks awaiting cleanup
//!
//! Each list is a `List_t { uxNumberOfItems, pxIndex, pxHead, pxTail }`; the
//! items are embedded `ListItem_t`s whose `pxNext`/`pvOwner` let us walk from
//! `pxHead->pxNext` back to `pxHead`. `pvOwner` is the owning TCB, which holds
//! `pcTaskName` (a char array), `uxPriority` and `xStateListItem` (the list
//! item that encodes the task's current state via which list contains it).
//!
//! Layout offsets differ by port/config, so the caller supplies a
//! [`RtosLayout`]. The defaults target a 32-bit Cortex-M build with
//! `configMAX_TASK_NAME_LEN = 16` and `configUSE_TRACE_FACILITY` disabled
//! (the common bare-metal STM32 configuration).
//!
//! The walker is pure and driven by a [`ReadFn`] closure so it can be unit
//! tested against synthetic memory without a target.

/// A memory reader: `(address, length) -> bytes`. Returns `None` on a read
/// that would leave the buffer short, so the walker bails cleanly.
pub type ReadFn<'a> = &'a dyn Fn(u64, usize) -> Option<Vec<u8>>;

/// Offsets (in bytes) into the FreeRTOS scheduler structures for a given
/// port/config. All offsets are relative to the containing struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtosLayout {
    /// Word size (4 for Cortex-M, 8 for 64-bit ports).
    pub word_size: usize,
    /// Offset of the TCB fields we read.
    pub tcb_pc_task_name: usize,
    pub tcb_ux_priority: usize,
    pub tcb_x_state_list_item: usize,
    /// Offset of `pvOwner` / `pxNext` inside a `ListItem_t`.
    pub list_item_pv_owner: usize,
    pub list_item_px_next: usize,
    /// Offset of the embedded `xListEnd` head `ListItem_t` inside a `List_t`.
    pub list_x_list_end: usize,
    pub list_ux_number_of_items: usize,
    /// `configMAX_TASK_NAME_LEN`.
    pub task_name_len: usize,
}

impl Default for RtosLayout {
    fn default() -> Self {
        // Common Cortex-M, 32-bit FreeRTOS:
        //   TCB: pxTopOfStack(0), xStateListItem(4), xEventListItem(20),
        //        uxPriority(36), pxStack(40), pcTaskName(44).
        //   ListItem_t: xItemValue(0), pxNext(4), pxPrevious(8), pvOwner(12).
        //   List_t: uxNumberOfItems(0), pxIndex(4), xListEnd(8) — the head is
        //        embedded at offset 8, not a pointer.
        Self {
            word_size: 4,
            tcb_pc_task_name: 44,
            tcb_ux_priority: 36,
            tcb_x_state_list_item: 4,
            list_item_pv_owner: 12,
            list_item_px_next: 4,
            list_x_list_end: 8,
            list_ux_number_of_items: 0,
            task_name_len: 16,
        }
    }
}


/// One task snapshot recovered from a real FreeRTOS kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtTask {
    pub name: String,
    pub priority: u32,
    /// `true` when this TCB is the current task (`pxCurrentTCB`).
    pub running: bool,
    /// 0 = ready, 1 = blocked, 2 = suspended, 3 = deleted.
    pub state: u32,
    /// Address of the TCB (for de-duplication across lists).
    pub tcb: u64,
}

/// Read a little-endian unsigned integer of `width` bytes.
fn read_uint(read: ReadFn, address: u64, width: usize) -> Option<u64> {
    let bytes = read(address, width)?;
    let mut value = 0u64;
    for (i, byte) in bytes.iter().enumerate() {
        value |= (*byte as u64) << (8 * i);
    }
    Some(value)
}

/// Read a NUL-terminated ASCII name from `pcTaskName`.
fn read_name(read: ReadFn, address: u64, len: usize) -> Option<String> {
    let bytes = read(address, len)?;
    let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
    let name = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
    Some(name)
}

/// Walk a single `List_t` whose head is at `list_head`, collecting each TCB.
///
/// The list head is a sentinel `ListItem_t`; `pxHead->pxNext` is the first
/// real item. Walking `pxNext` until we return to the head enumerates every
/// member exactly once. `state` is the task state implied by the list.
fn walk_list(
    read: ReadFn,
    layout: &RtosLayout,
    list_head: u64,
    state: u32,
    out: &mut Vec<RtTask>,
    running_tcb: Option<u64>,
) -> Option<()> {
    let _word = layout.word_size as u64;
    // List_t: uxNumberOfItems at offset 0; pxHead at list_px_head.
    let count = read_uint(read, list_head, layout.word_size)?;
    if count == 0 {
        return Some(());
    }
    // The head `ListItem_t` (`xListEnd`) is embedded in the `List_t` at
    // `list_x_list_end`; its `pxNext` is the first real item.
    let px_head = list_head + layout.list_x_list_end as u64;
    let first = read_uint(
        read,
        px_head + layout.list_item_px_next as u64,
        layout.word_size,
    )?;
    // Guard against a corrupt/empty list: the head points at itself.
    if first == px_head {
        return Some(());
    }


    let mut current = first;
    for _ in 0..count {
        if current == 0 || current == px_head {
            break;
        }
        let pv_owner = read_uint(
            read,
            current + layout.list_item_pv_owner as u64,
            layout.word_size,
        )?;
        if pv_owner != 0 {
            let name = read_name(
                read,
                pv_owner + layout.tcb_pc_task_name as u64,
                layout.task_name_len,
            )
            .unwrap_or_default();
            let priority = read_uint(
                read,
                pv_owner + layout.tcb_ux_priority as u64,
                layout.word_size,
            )
            .unwrap_or(0) as u32;
            if !name.is_empty() {
                out.push(RtTask {
                    name,
                    priority,
                    running: Some(pv_owner) == running_tcb,
                    state,
                    tcb: pv_owner,
                });
            }
        }
        let next = read_uint(
            read,
            current + layout.list_item_px_next as u64,
            layout.word_size,
        )?;
        if next == current {
            break;
        }
        current = next;
        let _ = _word;

    }
    Some(())
}

/// Enumerate all FreeRTOS tasks by walking the scheduler's intrusive lists.
///
/// `lists` holds the address of each list head, keyed by the state it implies:
/// ready lists (one per priority) carry state 0, the delayed lists 1, the
/// suspended list 2 and the termination list 3. `running_tcb` is the address
/// of the TCB `pxCurrentTCB` points at, used to mark the running task.
///
/// Returns the de-duplicated tasks (a task can appear in one list only, but
/// list traversal may revisit it if a list is empty or malformed).
pub fn walk_free_rtos(
    read: ReadFn,
    layout: &RtosLayout,
    lists: &[(u64, u32)],
    running_tcb: Option<u64>,
) -> Vec<RtTask> {
    let mut tasks: Vec<RtTask> = Vec::new();
    for (head, state) in lists {
        walk_list(read, layout, *head, *state, &mut tasks, running_tcb);
    }
    // De-duplicate by TCB address (keep the first, which has the correct
    // ready/blocked state for that list).
    let mut seen = std::collections::HashSet::new();
    tasks.retain(|task| seen.insert(task.tcb));
    tasks.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.name.cmp(&b.name)));
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic memory image with a couple of FreeRTOS tasks.
    struct Memory {
        data: Vec<u8>,
        base: u64,
    }

    impl Memory {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
                base: 0x2000_0000,
            }
        }

        fn addr(&self, offset: usize) -> u64 {
            self.base + offset as u64
        }

        fn write_u32(&mut self, offset: usize, value: u64) {
            let b = value.to_le_bytes();
            for (i, byte) in b.iter().enumerate() {
                self.data[offset + i] = *byte;
            }
        }

        fn write_str(&mut self, offset: usize, text: &str) {
            for (i, byte) in text.as_bytes().iter().enumerate() {
                self.data[offset + i] = *byte;
            }
        }

        fn read(&self, address: u64, len: usize) -> Option<Vec<u8>> {
            if address < self.base {
                return None;
            }
            let off = (address - self.base) as usize;
            if off + len > self.data.len() {
                return None;
            }
            Some(self.data[off..off + len].to_vec())
        }

    }

    #[test]
    fn walks_ready_and_delayed_lists_and_marks_running() {
        let layout = RtosLayout::default();
        let mut mem = Memory::new(0x4000);

        // Task A: TCB at 0x100, priority 1, ready list 0.
        let tcb_a = 0x100usize;
        mem.write_str(tcb_a + layout.tcb_pc_task_name, "taskA");
        mem.write_u32(tcb_a + layout.tcb_ux_priority, 1);

        // Task B: TCB at 0x200, priority 2, delayed list.
        let tcb_b = 0x200usize;
        mem.write_str(tcb_b + layout.tcb_pc_task_name, "taskB");
        mem.write_u32(tcb_b + layout.tcb_ux_priority, 2);

        // List 0 head (ready) at 0x1000: head item at 0x1010.
        let ready_head = 0x1000usize;
        let ready_item = 0x1010usize;
        mem.write_u32(ready_head + layout.list_ux_number_of_items, 1);
        // The embedded `xListEnd` head's pxNext points at the first item.
        mem.write_u32(
            ready_head + layout.list_x_list_end + layout.list_item_px_next,
            mem.addr(ready_item),
        );


        mem.write_u32(ready_item + layout.list_item_px_next, mem.addr(ready_head) + layout.list_x_list_end as u64);
        mem.write_u32(ready_item + layout.list_item_pv_owner, mem.addr(tcb_a));

        // Delayed list 1 head at 0x2000: one item (taskB).
        let delayed_head = 0x2000usize;
        let delayed_item = 0x2010usize;
        mem.write_u32(delayed_head + layout.list_ux_number_of_items, 1);
        mem.write_u32(
            delayed_head + layout.list_x_list_end + layout.list_item_px_next,
            mem.addr(delayed_item),
        );


        mem.write_u32(delayed_item + layout.list_item_px_next, mem.addr(delayed_head) + layout.list_x_list_end as u64);
        mem.write_u32(delayed_item + layout.list_item_pv_owner, mem.addr(tcb_b));

        let lists = vec![
            (mem.addr(ready_head), 0u32),
            (mem.addr(delayed_head), 1u32),
        ];
        let tasks = walk_free_rtos(&|addr, len| mem.read(addr, len), &layout, &lists, Some(mem.addr(tcb_a)));

        assert_eq!(tasks.len(), 2, "{tasks:?}");
        let a = tasks.iter().find(|t| t.name == "taskA").unwrap();
        assert!(a.running, "taskA is pxCurrentTCB");
        assert_eq!(a.state, 0);
        let b = tasks.iter().find(|t| t.name == "taskB").unwrap();
        assert!(!b.running);
        assert_eq!(b.state, 1);
    }

    #[test]
    fn empty_and_self_linked_lists_do_not_panic() {
        let layout = RtosLayout::default();
        let mut mem = Memory::new(0x1000);
        let head = 0x100usize;
        mem.write_u32(head + layout.list_ux_number_of_items, 0);
        let lists = vec![(mem.addr(head), 0u32)];
        assert!(walk_free_rtos(&|addr, len| mem.read(addr, len), &layout, &lists, None).is_empty());

        // Self-linked head (one item pointing back to itself) is a no-op.
        // Self-linked head (embedded xListEnd's pxNext points at itself).
        mem.write_u32(head + layout.list_ux_number_of_items, 1);
        mem.write_u32(
            head + layout.list_x_list_end + layout.list_item_px_next,
            mem.addr(head) + layout.list_x_list_end as u64,
        );

        assert!(walk_free_rtos(&|addr, len| mem.read(addr, len), &layout, &lists, None).is_empty());
    }

    #[test]
    fn deduplicates_tasks_seen_in_multiple_lists() {
        let layout = RtosLayout::default();
        let mut mem = Memory::new(0x3000);
        let tcb = 0x100usize;
        mem.write_str(tcb + layout.tcb_pc_task_name, "dup");
        mem.write_u32(tcb + layout.tcb_ux_priority, 0);

        let head = 0x1000usize;
        let item = 0x1010usize;
        mem.write_u32(head + layout.list_ux_number_of_items, 1);
        mem.write_u32(
            head + layout.list_x_list_end + layout.list_item_px_next,
            mem.addr(item),
        );
        mem.write_u32(item + layout.list_item_px_next, mem.addr(head) + layout.list_x_list_end as u64);
        mem.write_u32(item + layout.list_item_pv_owner, mem.addr(tcb));


        // Same TCB claimed by two lists; walker must report it once.
        let lists = vec![
            (mem.addr(head), 0u32),
            (mem.addr(head), 1u32),
        ];
        let tasks = walk_free_rtos(&|addr, len| mem.read(addr, len), &layout, &lists, None);
        assert_eq!(tasks.len(), 1, "{tasks:?}");
    }
}
