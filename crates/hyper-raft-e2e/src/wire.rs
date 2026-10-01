//! The datagrams members, clients and the test exchange, and the commands the log holds.
//!
//! Every datagram is `[crc32c][kind][body]`: the checksum covers the kind and the body and is
//! verified before anything is read, so a datagram that arrived damaged is dropped whole. A
//! body is read by [`Reader`], which refuses a length past what is there instead of reading it.
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
};

/// The most bytes one UDP datagram carries over IPv4: 65,535 for the whole IP packet (RFC 791)
/// less the 20-byte IP header and the 8-byte UDP header (RFC 768).
pub const MAX_DATAGRAM: usize = 65_507;
/// The bytes before a datagram's body: the 4-byte checksum and the kind.
pub const HEADER: usize = 5;
/// The CRC-32C (Castagnoli) polynomial, reflected (RFC 3720 Appendix B.4).
const CASTAGNOLI: u32 = 0x82f6_3b78;

/// CRC-32C of `bytes`, bit by bit (RFC 3720 Appendix B.4). The harness's datagrams are small
/// and few, so the table-free form is enough.
pub fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (CASTAGNOLI & mask);
        }
    }
    !crc
}

/// What a datagram is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A Raft message from one member to another.
    Raft,
    /// A client's request to a member.
    Request,
    /// A member's answer to a request.
    Response,
    /// The test's instruction to a member.
    Control,
}

impl Kind {
    fn byte(self) -> u8 {
        match self {
            Self::Raft => 1,
            Self::Request => 2,
            Self::Response => 3,
            Self::Control => 4,
        }
    }
    fn of(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Raft,
            2 => Self::Request,
            3 => Self::Response,
            4 => Self::Control,
            _ => return None,
        })
    }
}

/// Begins a datagram of `kind` in `buffer`, which keeps its capacity from one datagram to the
/// next.
pub fn begin(buffer: &mut Vec<u8>, kind: Kind) {
    buffer.clear();
    buffer.extend_from_slice(&[0; 4]);
    buffer.push(kind.byte());
}

/// The most bytes `socket` sends in one datagram. The OS may cap a datagram below what UDP can
/// say — macOS at `net.inet.udp.maxdgram`, 9,216 bytes by default (udp(4)) — and refuses a send
/// past the cap, so the cap is found by sending to the socket itself: a binary search between
/// nothing and [`MAX_DATAGRAM`], at most ⌈log2 MAX_DATAGRAM⌉ = 16 sends. Nothing waits for them
/// to arrive; each is of a kind no datagram has, and is read back out before the socket serves.
pub fn largest(socket: &UdpSocket) -> io::Result<usize> {
    let mut to = socket.local_addr()?;
    if to.ip().is_unspecified() {
        to.set_ip(match to.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        });
    }
    // Zeros but for a kind no datagram has.
    let mut buffer = vec![0u8; MAX_DATAGRAM];
    if let Some(kind) = buffer.get_mut(HEADER.saturating_sub(1)) {
        *kind = u8::MAX;
    }
    let (mut low, mut high) = (0usize, MAX_DATAGRAM);
    let mut sent = 0usize;
    while low < high {
        let size = high
            .checked_sub(low)
            .and_then(|span| span.checked_add(1))
            .map(|span| span / 2)
            .and_then(|half| low.checked_add(half))
            .unwrap_or(high);
        let went = buffer
            .get(..size)
            .is_some_and(|datagram| socket.send_to(datagram, to).is_ok());
        if went {
            low = size;
            sent = sent.saturating_add(1);
        } else {
            high = size.saturating_sub(1);
        }
    }
    drain(socket, sent, &mut buffer)?;
    Ok(low)
}

/// Takes the `sent` probes back out of `socket`, which would otherwise sit ahead of what peers
/// send. They went before this reads, so they come first, behind only what had already arrived;
/// a datagram of another's among them is dropped, as the network may drop it.
fn drain(socket: &UdpSocket, mut sent: usize, buffer: &mut [u8]) -> io::Result<()> {
    socket.set_nonblocking(true)?;
    let mut drained = Ok(());
    while sent > 0 {
        match socket.recv_from(buffer) {
            Ok((length, _)) => {
                if length >= HEADER && buffer.get(HEADER.saturating_sub(1)) == Some(&u8::MAX) {
                    sent = sent.saturating_sub(1);
                }
            }
            // Windows reports an ICMP port-unreachable for an earlier send on a receive.
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {}
            // A probe the socket did not keep.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => {
                drained = Err(error);
                break;
            }
        }
    }
    socket.set_nonblocking(false)?;
    drained
}

/// Ends the datagram in `buffer`: its checksum goes in front. False when it is longer than
/// `most`, the most the socket sends in one datagram ([`largest`]).
pub fn seal(buffer: &mut [u8], most: usize) -> bool {
    if buffer.len() > most.min(MAX_DATAGRAM) {
        return false;
    }
    let crc = crc32c(buffer.get(4..).unwrap_or(&[]));
    match buffer.get_mut(..4) {
        Some(head) => {
            head.copy_from_slice(&crc.to_le_bytes());
            true
        }
        None => false,
    }
}

/// The kind and body of a datagram whose checksum holds.
pub fn open(datagram: &[u8]) -> Option<(Kind, &[u8])> {
    if datagram.len() < HEADER {
        return None;
    }
    let crc = u32::from_le_bytes(datagram.get(..4)?.try_into().ok()?);
    let rest = datagram.get(4..)?;
    if crc32c(rest) != crc {
        return None;
    }
    let (kind, body) = rest.split_first()?;
    Some((Kind::of(*kind)?, body))
}

/// Puts a little-endian word.
pub fn put_u64(buffer: &mut Vec<u8>, value: u64) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

/// Puts bytes behind their 4-byte length. Bytes longer than a length can say are cut to it;
/// nothing here sends that many.
pub fn put_bytes(buffer: &mut Vec<u8>, bytes: &[u8]) {
    let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    buffer.extend_from_slice(&length.to_le_bytes());
    let taken = usize::try_from(length).unwrap_or(usize::MAX);
    buffer.extend_from_slice(bytes.get(..taken).unwrap_or(bytes));
}

/// Reads a body front to back; every read refuses what is not there.
pub struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    /// A reader of `bytes`.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.bytes.split_at_checked(count)?;
        self.bytes = rest;
        Some(taken)
    }
    /// One byte.
    pub fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }
    /// A little-endian word.
    pub fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    /// Bytes behind their 4-byte length.
    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        let length = u32::from_le_bytes(self.take(4)?.try_into().ok()?);
        self.take(usize::try_from(length).ok()?)
    }
    /// What is left.
    pub fn rest(self) -> &'a [u8] {
        self.bytes
    }
}

/// What a client asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op<'a> {
    /// Set `key` to `value`, answered once the write is committed and applied.
    Put {
        /// The key.
        key: &'a [u8],
        /// The value.
        value: &'a [u8],
    },
    /// The value of `key`, read linearizably (ReadIndex).
    Get {
        /// The key.
        key: &'a [u8],
    },
    /// What the member is.
    Status,
}

/// Puts a request.
pub fn put_request(buffer: &mut Vec<u8>, id: u64, op: &Op<'_>) {
    begin(buffer, Kind::Request);
    put_u64(buffer, id);
    match op {
        Op::Put { key, value } => {
            buffer.push(1);
            put_bytes(buffer, key);
            put_bytes(buffer, value);
        }
        Op::Get { key } => {
            buffer.push(2);
            put_bytes(buffer, key);
        }
        Op::Status => buffer.push(3),
    }
}

/// Reads a request's body.
pub fn read_request(body: &[u8]) -> Option<(u64, Op<'_>)> {
    let mut reader = Reader::new(body);
    let id = reader.u64()?;
    let op = match reader.u8()? {
        1 => Op::Put {
            key: reader.bytes()?,
            value: reader.bytes()?,
        },
        2 => Op::Get {
            key: reader.bytes()?,
        },
        3 => Op::Status,
        _ => return None,
    };
    Some((id, op))
}

/// What a member is, as it answers [`Op::Status`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// The member.
    pub id: u64,
    /// Its term.
    pub term: u64,
    /// Whether it leads.
    pub leads: bool,
    /// The leader it knows; zero for none.
    pub leader: u64,
    /// Its commit index.
    pub commit: u64,
    /// The index it applied through.
    pub applied: u64,
    /// The last index of its log.
    pub last_index: u64,
    /// A digest of everything it applied, in order.
    pub digest: u64,
}

/// A member's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The write is committed and applied at this index.
    Put(u64),
    /// The value read; none when the key holds none.
    Value(Option<Vec<u8>>),
    /// This member does not lead; the leader it knows, or zero.
    NotLeader(u64),
    /// Refused for room; ask again.
    Busy,
    /// What the member is.
    Status(Status),
    /// The instruction was carried out.
    Done,
}

/// Puts a response.
pub fn put_response(buffer: &mut Vec<u8>, id: u64, outcome: &Outcome) {
    begin(buffer, Kind::Response);
    put_u64(buffer, id);
    match outcome {
        Outcome::Put(index) => {
            buffer.push(1);
            put_u64(buffer, *index);
        }
        Outcome::Value(value) => {
            buffer.push(2);
            buffer.push(u8::from(value.is_some()));
            put_bytes(buffer, value.as_deref().unwrap_or(&[]));
        }
        Outcome::NotLeader(leader) => {
            buffer.push(3);
            put_u64(buffer, *leader);
        }
        Outcome::Busy => buffer.push(4),
        Outcome::Status(status) => {
            buffer.push(5);
            put_status(buffer, status);
        }
        Outcome::Done => buffer.push(6),
    }
}

fn put_status(buffer: &mut Vec<u8>, status: &Status) {
    for word in [
        status.id,
        status.term,
        u64::from(status.leads),
        status.leader,
        status.commit,
        status.applied,
        status.last_index,
        status.digest,
    ] {
        put_u64(buffer, word);
    }
}

fn read_status(reader: &mut Reader<'_>) -> Option<Status> {
    Some(Status {
        id: reader.u64()?,
        term: reader.u64()?,
        leads: reader.u64()? != 0,
        leader: reader.u64()?,
        commit: reader.u64()?,
        applied: reader.u64()?,
        last_index: reader.u64()?,
        digest: reader.u64()?,
    })
}

/// Reads a response's body.
pub fn read_response(body: &[u8]) -> Option<(u64, Outcome)> {
    let mut reader = Reader::new(body);
    let id = reader.u64()?;
    let outcome = match reader.u8()? {
        1 => Outcome::Put(reader.u64()?),
        2 => {
            let present = reader.u8()? != 0;
            let value = reader.bytes()?;
            Outcome::Value(present.then(|| value.to_vec()))
        }
        3 => Outcome::NotLeader(reader.u64()?),
        4 => Outcome::Busy,
        5 => Outcome::Status(read_status(&mut reader)?),
        6 => Outcome::Done,
        _ => return None,
    };
    Some((id, outcome))
}

/// The test's instruction to a member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    /// Where every member listens.
    Peers(Vec<(u64, SocketAddr)>),
    /// Drop every Raft datagram in and out (true), or stop dropping them (false): the member is
    /// cut off from the others, and clients and the test still reach it.
    Isolate(bool),
}

/// Puts an instruction.
pub fn put_control(buffer: &mut Vec<u8>, id: u64, control: &Control) {
    begin(buffer, Kind::Control);
    put_u64(buffer, id);
    match control {
        Control::Peers(peers) => {
            buffer.push(1);
            put_u64(buffer, u64::try_from(peers.len()).unwrap_or(u64::MAX));
            for (peer, address) in peers {
                put_u64(buffer, *peer);
                put_bytes(buffer, address.to_string().as_bytes());
            }
        }
        Control::Isolate(cut) => {
            buffer.push(2);
            buffer.push(u8::from(*cut));
        }
    }
}

/// Reads an instruction's body; at most `max_peers` peers.
pub fn read_control(body: &[u8], max_peers: usize) -> Option<(u64, Control)> {
    let mut reader = Reader::new(body);
    let id = reader.u64()?;
    let control = match reader.u8()? {
        1 => {
            let count = usize::try_from(reader.u64()?).ok()?;
            if count > max_peers {
                return None;
            }
            let mut peers = Vec::with_capacity(count);
            for _ in 0..count {
                let peer = reader.u64()?;
                let address = std::str::from_utf8(reader.bytes()?).ok()?.parse().ok()?;
                peers.push((peer, address));
            }
            Control::Peers(peers)
        }
        2 => Control::Isolate(reader.u8()? != 0),
        _ => return None,
    };
    Some((id, control))
}

/// A write as the log holds it: who proposed it, under which of its numbers, and what it sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command<'a> {
    /// The member that proposed it, which answers its client once it is applied.
    pub origin: u64,
    /// The proposer's number for it.
    pub sequence: u64,
    /// The key.
    pub key: &'a [u8],
    /// The value.
    pub value: &'a [u8],
}

/// Puts a command, as an entry's data.
pub fn put_command(buffer: &mut Vec<u8>, command: &Command<'_>) {
    buffer.clear();
    put_u64(buffer, command.origin);
    put_u64(buffer, command.sequence);
    put_bytes(buffer, command.key);
    put_bytes(buffer, command.value);
}

/// Reads a command from an entry's data.
pub fn read_command(data: &[u8]) -> Option<Command<'_>> {
    let mut reader = Reader::new(data);
    Some(Command {
        origin: reader.u64()?,
        sequence: reader.u64()?,
        key: reader.bytes()?,
        value: reader.bytes()?,
    })
}
