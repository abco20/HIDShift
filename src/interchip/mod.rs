pub mod cell;
pub mod message;
pub mod record;
pub mod reliable;

pub use cell::{
    SPI_CELL_HEADER_LEN, SPI_CELL_LEN, SPI_CELL_MAGIC, SPI_CELL_PAYLOAD_LEN, SPI_PROTOCOL_VERSION,
    SpiCell, SpiCellError, SpiCellHeader,
};
pub use message::{Hello, InterchipRole, StandardInputReport, StandardInputReportError};
pub use record::{Record, RecordCodecError, RecordIter, RecordRef, encode_records};
pub use reliable::{
    ReceiveDisposition, ReliableReceiver, ReliableSender, RetransmitAction, SPI_TX_WINDOW,
    SenderError,
};
