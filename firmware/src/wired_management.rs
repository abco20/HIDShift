use hidshift::management::{MANAGEMENT_REQUEST_LEN, ManagementRequest, ManagementResponse};

pub const REQUEST_PREFIX: &[u8] = b"@HIDSHIFT:";
pub const REQUEST_LINE_LEN: usize = REQUEST_PREFIX.len() + MANAGEMENT_REQUEST_LEN * 2;

pub fn decode_request_line(line: &[u8]) -> Option<ManagementRequest> {
    if line.len() != REQUEST_LINE_LEN || !line.starts_with(REQUEST_PREFIX) {
        return None;
    }
    let encoded = &line[REQUEST_PREFIX.len()..];
    let mut request = [0u8; MANAGEMENT_REQUEST_LEN];
    for (index, output) in request.iter_mut().enumerate() {
        let high = hex_nibble(encoded[index * 2])?;
        let low = hex_nibble(encoded[index * 2 + 1])?;
        *output = (high << 4) | low;
    }
    ManagementRequest::decode(&request).ok()
}

pub fn print_response(response: ManagementResponse) {
    let bytes = response.encode();
    esp_println::println!(
        "@HIDSHIFT:{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
        bytes[16],
        bytes[17],
        bytes[18],
        bytes[19]
    );
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
