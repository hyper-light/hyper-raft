//! Native AIO reads (`hyper_block::aio`) on a real file opened for direct I/O. On Linux they read
//! through the kernel's AIO; elsewhere, and over a buffered file, they are refused, typed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation
)]

use hyper_block::DiskError;
use hyper_block::aio::AioReads;
use hyper_block::buf::{AlignedBuf, Alignment};
#[cfg(target_os = "linux")]
use hyper_block::file::Caching;
use hyper_block::file::{CachingRequest, DeviceFile};

/// The page every read in these tests moves: the block of the file systems they run on.
const PAGE: usize = 4096;

fn align() -> Alignment {
    Alignment::new(PAGE).unwrap()
}

/// The byte page `i` of the test file is filled with.
fn fill(i: usize) -> u8 {
    u8::try_from(i % 251).unwrap() + 1
}

/// A file of `pages` pages, page `i` filled with `fill(i)`, flushed, opened as `request` asks.
fn file(dir: &std::path::Path, pages: usize, request: CachingRequest) -> DeviceFile {
    let path = dir.join("aio");
    let file = DeviceFile::open(&path, true, request, align()).unwrap();
    let mut buf = AlignedBuf::zeroed(PAGE, align()).unwrap();
    for i in 0..pages {
        buf.clear();
        buf.extend_from_slice(&[fill(i); PAGE]).unwrap();
        file.write_all_at(buf.as_slice(), (i * PAGE) as u64)
            .unwrap();
    }
    file.sync_data().unwrap();
    file
}

#[cfg(target_os = "linux")]
fn reads(pages: &[usize]) -> Vec<(AlignedBuf, u64)> {
    pages
        .iter()
        .map(|&i| {
            let mut buf = AlignedBuf::zeroed(PAGE, align()).unwrap();
            buf.set_len(PAGE).unwrap();
            (buf, (i * PAGE) as u64)
        })
        .collect()
}

/// Do: open AIO reads over a buffered file. Expect: refused, typed, since a buffered AIO read is
/// done inside io_submit; on every OS but Linux, refused over any file.
#[test]
fn a_buffered_file_or_another_os_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let buffered = file(dir.path(), 1, CachingRequest::Buffered);
    assert!(matches!(
        AioReads::new(buffered, 4, 1),
        Err(DiskError::Unsupported { .. })
    ));
    if cfg!(not(target_os = "linux")) {
        let direct = DeviceFile::open(
            &dir.path().join("direct"),
            true,
            CachingRequest::PreferDirect,
            align(),
        )
        .unwrap();
        assert!(matches!(
            AioReads::new(direct, 4, 1),
            Err(DiskError::Unsupported { .. })
        ));
    }
}

/// The direct file, on Linux, where the file system takes direct I/O (the test's directory may
/// be on one that does not, tmpfs among them; then there is nothing to read through AIO).
#[cfg(target_os = "linux")]
fn direct(dir: &std::path::Path, pages: usize) -> Option<DeviceFile> {
    let file = file(dir, pages, CachingRequest::PreferDirect);
    (file.caching() == Caching::Direct).then_some(file)
}

/// Do: read 8 pages in one batch at a depth of 4, so half wait for the first half. Expect: every
/// buffer comes back in the order given, holding its page's bytes.
#[cfg(target_os = "linux")]
#[test]
fn a_batch_deeper_than_the_context_reads_every_page_back_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let Some(file) = direct(dir.path(), 64) else {
        return;
    };
    let mut aio = AioReads::new(file, 4, 2).unwrap();
    let pages = [3, 17, 0, 63, 9, 40, 22, 5];
    let number = aio.submit_reads(reads(&pages)).unwrap();
    let (answered, done) = aio.answer().unwrap();
    assert_eq!(answered, number);
    let done = done.unwrap();
    assert_eq!(done.len(), pages.len());
    for ((buf, at), &page) in done.iter().zip(&pages) {
        assert_eq!(*at, (page * PAGE) as u64);
        assert!(
            buf.as_slice().iter().all(|&b| b == fill(page)),
            "page {page}"
        );
    }
    assert_eq!(aio.out(), 0);
}

/// Do: submit two batches, then poll with try_answer until both are answered. Expect: both
/// answered, each with its own pages, and try_answer with nothing out answers none.
#[cfg(target_os = "linux")]
#[test]
fn two_batches_out_at_once_are_both_answered_by_polling() {
    let dir = tempfile::tempdir().unwrap();
    let Some(file) = direct(dir.path(), 16) else {
        return;
    };
    let mut aio = AioReads::new(file, 8, 2).unwrap();
    let first = aio.submit_reads(reads(&[1, 2, 3])).unwrap();
    let second = aio.submit_reads(reads(&[10, 11])).unwrap();
    let mut answers = Vec::new();
    // Polling ends at the fact it needs, both answers, each poll a non-blocking reap.
    while answers.len() < 2 {
        if let Some(answer) = aio.try_answer().unwrap() {
            answers.push(answer);
        }
    }
    answers.sort_by_key(|(n, _)| *n);
    let pages: Vec<Vec<u8>> = answers
        .into_iter()
        .map(|(_, a)| a.unwrap().iter().map(|(b, _)| b.as_slice()[0]).collect())
        .collect();
    assert_eq!(
        pages,
        [vec![fill(1), fill(2), fill(3)], vec![fill(10), fill(11)]]
    );
    let _ = (first, second);
    assert!(aio.try_answer().unwrap().is_none());
}

/// Do: read a page past the end of the file beside one inside it. Expect: the batch fails as a
/// whole, typed (a short read), and nothing is left out.
#[cfg(target_os = "linux")]
#[test]
fn a_read_past_the_end_fails_its_batch() {
    let dir = tempfile::tempdir().unwrap();
    let Some(file) = direct(dir.path(), 2) else {
        return;
    };
    let mut aio = AioReads::new(file, 4, 1).unwrap();
    aio.submit_reads(reads(&[0, 8])).unwrap();
    let (_, answer) = aio.answer().unwrap();
    assert!(matches!(answer, Err(DiskError::ShortRead { .. })));
    assert_eq!(aio.out(), 0);
}

/// Do: submit past the batches allowed out, then a misaligned read. Expect: each refused, typed,
/// before anything is submitted, its reads handed back.
#[cfg(target_os = "linux")]
#[test]
fn a_batch_past_the_bound_or_misaligned_is_refused_and_given_back() {
    let dir = tempfile::tempdir().unwrap();
    let Some(file) = direct(dir.path(), 4) else {
        return;
    };
    let mut aio = AioReads::new(file, 4, 1).unwrap();
    aio.submit_reads(reads(&[0])).unwrap();
    let (refused, given_back) = aio.submit_reads(reads(&[1, 2])).unwrap_err();
    assert!(matches!(refused, DiskError::Unsupported { .. }));
    assert_eq!(given_back.len(), 2);
    aio.answer().unwrap().1.unwrap();
    let mut odd = reads(&[0]);
    odd[0].1 = 512;
    let (refused, _) = aio.submit_reads(odd).unwrap_err();
    assert!(matches!(refused, DiskError::Misaligned { .. }));
    assert_eq!(aio.out(), 0);
}

/// Do: submit a batch and drop the reads with it still out. Expect: the drop waits for the kernel
/// (io_destroy blocks on completion), so no buffer is freed under a read; nothing panics.
#[cfg(target_os = "linux")]
#[test]
fn dropping_with_reads_out_waits_for_them() {
    let dir = tempfile::tempdir().unwrap();
    let Some(file) = direct(dir.path(), 32) else {
        return;
    };
    let mut aio = AioReads::new(file, 16, 1).unwrap();
    aio.submit_reads(reads(&(0..32).collect::<Vec<_>>()))
        .unwrap();
    drop(aio);
}
