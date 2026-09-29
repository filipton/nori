//! A PyTorch checkpoint's float32 tensors, read without Python: a stored zip holding `<name>/data.pkl` and each
//! storage as `<name>/data/<key>`. The pickle runs on a restricted unpickler that knows only a state_dict's values,
//! `OrderedDict`, storages and `_rebuild_tensor_v2`, and refuses every other opcode and global. Every count and
//! offset is checked.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// A float32 tensor of the state_dict, its values in row-major order.
#[derive(Debug, Clone, PartialEq)]
pub struct Tensor {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

impl Tensor {
    pub fn numel(&self) -> usize {
        self.data.len()
    }
}

/// The state_dict's float32 tensors by key, Lightning's "model." prefix removed; other tensors are checked and
/// left out.
pub fn state_dict(ckpt: &[u8]) -> Result<HashMap<String, Tensor>, String> {
    let zip = entries(ckpt)?;
    let pkl = zip.keys().filter(|n| n.ends_with("/data.pkl") && n.matches('/').count() == 1).collect::<Vec<_>>();
    let [pkl] = pkl[..] else { return Err("not one data.pkl in the archive".into()) };
    let dir = &pkl[..pkl.len() - "data.pkl".len()];
    if let Some(order) = zip.get(format!("{dir}byteorder").as_str()) {
        if *order != b"little" {
            return Err("the tensors are not little-endian".into());
        }
    }
    let storage = |key: &str| zip.get(format!("{dir}data/{key}").as_str()).copied();
    let top = Machine::new(zip[pkl], &storage).run()?;
    let V::Dict(top) = top else { return Err("the checkpoint is not a dict".into()) };
    let top = top.borrow();
    let Some((_, V::Dict(sd))) = top.iter().find(|(k, _)| matches!(k, V::Str(s) if &**s == "state_dict")) else {
        return Err("the checkpoint has no state_dict".into());
    };
    let mut out = HashMap::new();
    for (k, v) in sd.borrow().iter() {
        let V::Str(k) = k else { return Err("a state_dict key that is not a string".into()) };
        let key = k.strip_prefix("model.").unwrap_or(k).to_string();
        match v {
            V::Tensor(Some(t)) => {
                out.insert(key, (**t).clone());
            }
            V::Tensor(None) => {}
            _ => return Err(format!("{k} is not a tensor")),
        }
    }
    Ok(out)
}

/// The stored entries of a zip archive by name, sizes from the central directory. Deflated entries and ZIP64-only
/// archives are refused.
fn entries(b: &[u8]) -> Result<HashMap<&str, &[u8]>, String> {
    let u16_at = |p: usize| -> Result<usize, String> { b.get(p..p + 2).map(|s| u16::from_le_bytes([s[0], s[1]]) as usize).ok_or_else(|| "the archive is cut short".to_string()) };
    let u32_at = |p: usize| -> Result<usize, String> { b.get(p..p + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize).ok_or_else(|| "the archive is cut short".to_string()) };
    // End of central directory: 22 bytes plus a comment of up to 64 kB.
    if b.len() < 22 {
        return Err("not a zip archive".into());
    }
    let low = b.len().saturating_sub(22 + 0xFFFF);
    let eocd = (low..=b.len().saturating_sub(22)).rev().find(|&p| b[p..p + 4] == [0x50, 0x4b, 0x05, 0x06]).ok_or("not a zip archive")?;
    let (count, cd_len, cd_at) = (u16_at(eocd + 10)?, u32_at(eocd + 12)?, u32_at(eocd + 16)?);
    if count == 0xFFFF || cd_len == 0xFFFF_FFFF || cd_at == 0xFFFF_FFFF {
        return Err("a ZIP64 archive".into());
    }
    let mut out = HashMap::new();
    let mut p = cd_at;
    for _ in 0..count {
        if u32_at(p)? != 0x0201_4b50 {
            return Err("a broken central directory".into());
        }
        let (method, size, stored) = (u16_at(p + 10)?, u32_at(p + 20)?, u32_at(p + 24)?);
        let (name_len, extra_len, comment_len, local) = (u16_at(p + 28)?, u16_at(p + 30)?, u16_at(p + 32)?, u32_at(p + 42)?);
        let name = b.get(p + 46..p + 46 + name_len).ok_or("the archive is cut short")?;
        let name = std::str::from_utf8(name).map_err(|_| "an entry's name is not UTF-8")?;
        if method != 0 || size != stored || size == 0xFFFF_FFFF {
            return Err(format!("{name} is compressed or too big"));
        }
        if u32_at(local)? != 0x0403_4b50 {
            return Err(format!("{name} has no local header"));
        }
        let start = local + 30 + u16_at(local + 26)? + u16_at(local + 28)?;
        let data = b.get(start..start.checked_add(size).ok_or("an entry past the end")?).ok_or("an entry past the end")?;
        if out.insert(name, data).is_some() {
            return Err(format!("{name} is in the archive twice"));
        }
        p += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// The globals a checkpoint may name; anything else is refused.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Global {
    OrderedDict,
    RebuildTensor,
    Storage(Kind),
}

/// A storage's element type. Only float32 tensors are kept; the others are checked and dropped.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    F32,
    I64,
}

impl Kind {
    fn size(self) -> usize {
        match self {
            Kind::F32 => 4,
            Kind::I64 => 8,
        }
    }
}

#[derive(Debug, Clone)]
enum V {
    None,
    /// Bools and floats are hyperparameters or `requires_grad`; their values are not needed.
    Bool,
    Int(i64),
    Float,
    Str(Rc<str>),
    Tuple(Rc<[V]>),
    List(Rc<RefCell<Vec<V>>>),
    Dict(Rc<RefCell<Vec<(V, V)>>>),
    Global(Global),
    /// A persistent id: a tensor storage's type and its file's key.
    Storage(Kind, Rc<str>),
    /// A tensor; `None` for one that is not float32.
    Tensor(Option<Rc<Tensor>>),
}

/// The restricted unpickler: a stack, marks, the memo, and a cursor over the pickle.
struct Machine<'a> {
    b: &'a [u8],
    at: usize,
    stack: Vec<V>,
    marks: Vec<usize>,
    memo: HashMap<u32, V>,
    storage: &'a dyn Fn(&str) -> Option<&'a [u8]>,
}

impl<'a> Machine<'a> {
    fn new(b: &'a [u8], storage: &'a dyn Fn(&str) -> Option<&'a [u8]>) -> Self {
        Machine { b, at: 0, stack: Vec::new(), marks: Vec::new(), memo: HashMap::new(), storage }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self.b.get(self.at..self.at.checked_add(n).ok_or("a length past the end")?).ok_or("the pickle is cut short")?;
        self.at += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn pop(&mut self) -> Result<V, String> {
        if self.marks.last().is_some_and(|m| *m >= self.stack.len()) {
            return Err("popped past a mark".into());
        }
        self.stack.pop().ok_or_else(|| "popped an empty stack".into())
    }

    fn top(&mut self) -> Result<&mut V, String> {
        if self.marks.last().is_some_and(|m| *m >= self.stack.len()) {
            return Err("read past a mark".into());
        }
        self.stack.last_mut().ok_or_else(|| "read an empty stack".into())
    }

    /// Everything above the last mark, and the mark gone.
    fn pop_to_mark(&mut self) -> Result<Vec<V>, String> {
        let m = self.marks.pop().ok_or("no mark")?;
        Ok(self.stack.split_off(m))
    }

    fn string(&mut self, n: usize) -> Result<V, String> {
        let s = std::str::from_utf8(self.take(n)?).map_err(|_| "a string that is not UTF-8")?;
        Ok(V::Str(s.into()))
    }

    fn run(mut self) -> Result<V, String> {
        loop {
            let op = self.u8()?;
            match op {
                0x80 => {
                    // PROTO
                    if self.u8()? > 5 {
                        return Err("a pickle protocol above 5".into());
                    }
                }
                0x95 => {
                    // FRAME: only a hint of how much follows.
                    self.take(8)?;
                }
                b'.' => {
                    // STOP
                    let v = self.pop()?;
                    return if self.stack.is_empty() && self.marks.is_empty() { Ok(v) } else { Err("left over at the end".into()) };
                }
                b'(' => self.marks.push(self.stack.len()),
                b'N' => self.stack.push(V::None),
                0x88 => self.stack.push(V::Bool),
                0x89 => self.stack.push(V::Bool),
                b'J' => {
                    let v = self.u32()? as i32;
                    self.stack.push(V::Int(v as i64));
                }
                b'K' => {
                    let v = self.u8()?;
                    self.stack.push(V::Int(v as i64));
                }
                b'M' => {
                    let s = self.take(2)?;
                    self.stack.push(V::Int(u16::from_le_bytes([s[0], s[1]]) as i64));
                }
                0x8a => {
                    // LONG1: a little-endian two's complement integer of up to 8 bytes here.
                    let n = self.u8()? as usize;
                    if n > 8 {
                        return Err("an integer too long".into());
                    }
                    let s = self.take(n)?;
                    let mut v = 0i64;
                    for (i, byte) in s.iter().enumerate() {
                        v |= (*byte as i64) << (8 * i);
                    }
                    if n > 0 && n < 8 && s[n - 1] & 0x80 != 0 {
                        v -= 1i64 << (8 * n);
                    }
                    self.stack.push(V::Int(v));
                }
                b'G' => {
                    self.take(8)?;
                    self.stack.push(V::Float);
                }
                b'X' => {
                    let n = self.u32()? as usize;
                    let v = self.string(n)?;
                    self.stack.push(v);
                }
                0x8c => {
                    let n = self.u8()? as usize;
                    let v = self.string(n)?;
                    self.stack.push(v);
                }
                b')' => self.stack.push(V::Tuple(Rc::from(Vec::new()))),
                b't' => {
                    let items = self.pop_to_mark()?;
                    self.stack.push(V::Tuple(items.into()));
                }
                0x85..=0x87 => {
                    let n = (op - 0x84) as usize;
                    let mut items = Vec::with_capacity(n);
                    for _ in 0..n {
                        items.push(self.pop()?);
                    }
                    items.reverse();
                    self.stack.push(V::Tuple(items.into()));
                }
                b']' => self.stack.push(V::List(Rc::new(RefCell::new(Vec::new())))),
                b'}' => self.stack.push(V::Dict(Rc::new(RefCell::new(Vec::new())))),
                b'a' => {
                    let v = self.pop()?;
                    let V::List(l) = self.top()? else { return Err("APPEND to something not a list".into()) };
                    l.borrow_mut().push(v);
                }
                b'e' => {
                    let items = self.pop_to_mark()?;
                    let V::List(l) = self.top()? else { return Err("APPENDS to something not a list".into()) };
                    l.borrow_mut().extend(items);
                }
                b's' => {
                    let v = self.pop()?;
                    let k = self.pop()?;
                    let V::Dict(d) = self.top()? else { return Err("SETITEM on something not a dict".into()) };
                    d.borrow_mut().push((k, v));
                }
                b'u' => {
                    let items = self.pop_to_mark()?;
                    if items.len() % 2 != 0 {
                        return Err("SETITEMS with a key and no value".into());
                    }
                    let V::Dict(d) = self.top()? else { return Err("SETITEMS on something not a dict".into()) };
                    let mut d = d.borrow_mut();
                    let mut it = items.into_iter();
                    while let (Some(k), Some(v)) = (it.next(), it.next()) {
                        d.push((k, v));
                    }
                }
                b'q' => {
                    let i = self.u8()? as u32;
                    let v = self.top()?.clone();
                    self.memo.insert(i, v);
                }
                b'r' => {
                    let i = self.u32()?;
                    let v = self.top()?.clone();
                    self.memo.insert(i, v);
                }
                0x94 => {
                    // MEMOIZE
                    let i = self.memo.len() as u32;
                    let v = self.top()?.clone();
                    self.memo.insert(i, v);
                }
                b'h' | b'j' => {
                    let i = if op == b'h' { self.u8()? as u32 } else { self.u32()? };
                    let v = self.memo.get(&i).cloned().ok_or("a memo entry that was never put")?;
                    self.stack.push(v);
                }
                b'c' => {
                    let module = self.line()?;
                    let name = self.line()?;
                    let g = match (module, name) {
                        ("collections", "OrderedDict") => Global::OrderedDict,
                        ("torch._utils", "_rebuild_tensor_v2") => Global::RebuildTensor,
                        ("torch", "FloatStorage") => Global::Storage(Kind::F32),
                        ("torch", "LongStorage") => Global::Storage(Kind::I64),
                        _ => return Err(format!("refused: {module}.{name}")),
                    };
                    self.stack.push(V::Global(g));
                }
                b'Q' => {
                    // BINPERSID: ('storage', <type>, key, location, numel)
                    let pid = self.pop()?;
                    let V::Tuple(t) = pid else { return Err("a persistent id that is not a tuple".into()) };
                    match &t[..] {
                        [V::Str(tag), V::Global(Global::Storage(kind)), V::Str(key), V::Str(_), V::Int(_)] if &**tag == "storage" => {
                            self.stack.push(V::Storage(*kind, key.clone()))
                        }
                        _ => return Err("a persistent id that is not a storage".into()),
                    }
                }
                b'R' => {
                    let args = self.pop()?;
                    let f = self.pop()?;
                    let V::Tuple(args) = args else { return Err("REDUCE without a tuple".into()) };
                    let v = match f {
                        V::Global(Global::OrderedDict) if args.is_empty() => V::Dict(Rc::new(RefCell::new(Vec::new()))),
                        V::Global(Global::RebuildTensor) => V::Tensor(self.tensor(&args)?),
                        _ => return Err("REDUCE of something other than a tensor or an OrderedDict".into()),
                    };
                    self.stack.push(v);
                }
                _ => return Err(format!("refused: opcode {op:#04x}")),
            }
        }
    }

    /// GLOBAL's module or name: up to a newline.
    fn line(&mut self) -> Result<&'a str, String> {
        let rest = &self.b[self.at..];
        let n = rest.iter().position(|c| *c == b'\n').ok_or("a GLOBAL without its newline")?;
        let s = std::str::from_utf8(&rest[..n]).map_err(|_| "a GLOBAL that is not UTF-8")?;
        self.at += n + 1;
        Ok(s)
    }

    /// `_rebuild_tensor_v2(storage, offset, size, stride, ...)`: the values read through the strides, indices checked.
    fn tensor(&self, args: &[V]) -> Result<Option<Rc<Tensor>>, String> {
        let [V::Storage(kind, key), V::Int(offset), V::Tuple(size), V::Tuple(stride), V::Bool, V::Dict(hooks), ..] = args else {
            return Err("a tensor rebuilt from something else".into());
        };
        if args.len() > 7 || !hooks.borrow().is_empty() {
            return Err("a tensor with hooks or more".into());
        }
        let ints = |t: &[V]| -> Result<Vec<usize>, String> {
            t.iter().map(|v| match v {
                V::Int(i) if *i >= 0 => Ok(*i as usize),
                _ => Err("a size or stride that is not a count".to_string()),
            }).collect()
        };
        let (shape, stride) = (ints(size)?, ints(stride)?);
        if shape.len() != stride.len() || *offset < 0 {
            return Err("a tensor's sizes and strides disagree".into());
        }
        let bytes = (self.storage)(key).ok_or_else(|| format!("storage {key} is not in the archive"))?;
        let have = bytes.len() / kind.size();
        let numel = shape.iter().try_fold(1usize, |a, s| a.checked_mul(*s)).ok_or("a tensor too big")?;
        let mut last = *offset as usize;
        for (s, st) in shape.iter().zip(&stride) {
            if *s > 0 {
                last = last.checked_add((s - 1).checked_mul(*st).ok_or("a tensor too big")?).ok_or("a tensor too big")?;
            }
        }
        // No more values than the storage holds (a checkpoint has no broadcast tensors), and none past its end.
        if numel > have || (numel > 0 && last >= have) {
            return Err(format!("a tensor past the end of storage {key}"));
        }
        if *kind != Kind::F32 {
            return Ok(None);
        }
        let mut data = Vec::with_capacity(numel);
        let mut index = vec![0usize; shape.len()];
        for _ in 0..numel {
            let at = *offset as usize + index.iter().zip(&stride).map(|(i, s)| i * s).sum::<usize>();
            let b = &bytes[at * 4..at * 4 + 4];
            data.push(f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            // The next index, the last dimension fastest.
            for d in (0..shape.len()).rev() {
                index[d] += 1;
                if index[d] < shape[d] {
                    break;
                }
                index[d] = 0;
            }
        }
        Ok(Some(Rc::new(Tensor { shape, data })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zip of stored entries, as torch.save writes one (sizes only in the central directory).
    fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in files {
            let local = out.len() as u32;
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&[20, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&[0; 8]);
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&3u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(b"pad");
            out.extend_from_slice(data);
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&[20, 0, 20, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0; 12]);
            central.extend_from_slice(&local.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let at = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&[0; 2]);
        out
    }

    fn unicode(s: &str) -> Vec<u8> {
        [&[b'X'][..], &(s.len() as u32).to_le_bytes(), s.as_bytes()].concat()
    }

    /// `_rebuild_tensor_v2` of storage `key` (float32 unless `long`), as torch pickles it.
    fn tensor(key: &str, long: bool, offset: u8, size: &[u8], stride: &[u8]) -> Vec<u8> {
        let mut p = b"ctorch._utils\n_rebuild_tensor_v2\n((".to_vec();
        p.extend(unicode("storage"));
        p.extend_from_slice(if long { b"ctorch\nLongStorage\n" } else { b"ctorch\nFloatStorage\n" });
        p.extend(unicode(key));
        p.extend(unicode("cpu"));
        p.extend_from_slice(&[b'K', 6, b't', b'Q', b'K', offset, b'(']);
        for s in size {
            p.extend_from_slice(&[b'K', *s]);
        }
        p.extend_from_slice(b"t(");
        for s in stride {
            p.extend_from_slice(&[b'K', *s]);
        }
        p.extend_from_slice(b"t\x89ccollections\nOrderedDict\n)Rtq\x07R");
        p
    }

    /// {"epoch": 3, "state_dict": OrderedDict(model.w: 2x3 from offset 0, model.t: the same storage transposed
    /// from offset 1, model.n: a long)}
    fn checkpoint() -> Vec<u8> {
        let mut p = b"\x80\x02}q\x00(".to_vec();
        p.extend(unicode("epoch"));
        p.extend_from_slice(b"K\x03");
        p.extend(unicode("lr"));
        p.extend_from_slice(b"G?\x50\x62\x4d\xd2\xf1\xa9\xfc");
        p.extend(unicode("state_dict"));
        p.extend_from_slice(b"ccollections\nOrderedDict\n)Rq\x01(");
        p.extend(unicode("model.w"));
        p.extend(tensor("0", false, 0, &[2, 3], &[3, 1]));
        p.extend(unicode("model.t"));
        p.extend(tensor("0", false, 1, &[2, 2], &[1, 3]));
        p.extend(unicode("model.n"));
        p.extend(tensor("1", true, 0, &[], &[]));
        p.extend_from_slice(b"uu.");
        let floats: Vec<u8> = (0..6).flat_map(|i| (i as f32 + 0.5).to_le_bytes()).collect();
        zip(&[("small/data.pkl", &p), ("small/byteorder", b"little"), ("small/data/0", &floats), ("small/data/1", &7i64.to_le_bytes())])
    }

    #[test]
    fn state_dict_is_read_through_strides() {
        let sd = state_dict(&checkpoint()).unwrap();
        assert_eq!(sd.len(), 2, "the long tensor is left out: {:?}", sd.keys());
        assert_eq!(sd["w"], Tensor { shape: vec![2, 3], data: vec![0.5, 1.5, 2.5, 3.5, 4.5, 5.5] });
        // From offset 1, column by column: [[1.5, 4.5], [2.5, 5.5]].
        assert_eq!(sd["t"], Tensor { shape: vec![2, 2], data: vec![1.5, 4.5, 2.5, 5.5] });
    }

    #[test]
    fn anything_but_values_is_refused() {
        let ok = checkpoint();
        // os.system, and a known global called with arguments it does not take.
        for (from, to) in [(&b"collections\nOrderedDict"[..], &b"os\nsystem\nXXXXXXXXXXXXX"[..]), (b"ctorch\nLongStorage", b"ctorch\nHalfStorage")] {
            let at = ok.windows(from.len()).position(|w| w == from).unwrap();
            let mut bad = ok.clone();
            bad.splice(at..at + from.len(), to.iter().copied());
            assert!(state_dict(&bad).is_err());
        }
        // BUILD, INST and a tensor reaching past its storage.
        let pickle = |extra: &[u8]| {
            let mut p = b"\x80\x02}q\x00(".to_vec();
            p.extend_from_slice(extra);
            p.extend_from_slice(b"u.");
            zip(&[("a/data.pkl", &p), ("a/data/0", &[0u8; 8])])
        };
        assert!(state_dict(&pickle(b"K\x01K\x02b")).unwrap_err().contains("0x62"));
        assert!(state_dict(&pickle(b"ios\nsystem\n")).unwrap_err().contains("0x69"));
        let mut past = unicode("state_dict");
        past.extend_from_slice(b"}(");
        past.extend(unicode("w"));
        past.extend(tensor("0", false, 1, &[2], &[1]));
        past.extend_from_slice(b"u");
        assert!(state_dict(&pickle(&past)).unwrap_err().contains("past the end"));
        let mut fits = unicode("state_dict");
        fits.extend_from_slice(b"}(");
        fits.extend(unicode("w"));
        fits.extend(tensor("0", false, 0, &[2], &[1]));
        fits.extend_from_slice(b"u");
        assert_eq!(state_dict(&pickle(&fits)).unwrap()["w"].data, vec![0.0, 0.0]);
        // Cut anywhere, it fails rather than panics.
        for n in 0..ok.len() {
            let _ = state_dict(&ok[..n]);
        }
        assert!(state_dict(b"not a zip at all, not even close to one").is_err());
    }
}
