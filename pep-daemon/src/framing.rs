use std::io::{self, Read, Write};

pub fn read_frame<R: Read>(stream: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

pub fn write_frame<W: Write>(stream: &mut W, data: &[u8]) -> io::Result<()> {
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(data)?;
    stream.flush()?;
    Ok(())
}
