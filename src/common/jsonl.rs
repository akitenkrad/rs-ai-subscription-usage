use std::io::BufRead;
pub fn read_lines(reader: impl BufRead) -> impl Iterator<Item = std::io::Result<String>> {
    reader.lines()
}
