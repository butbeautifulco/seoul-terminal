use libghostty_vt::terminal::{
    ColorScheme, ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType,
    PrimaryDeviceAttributes, SecondaryDeviceAttributes, SizeReportSize, TertiaryDeviceAttributes,
};

use crate::terminal::TerminalBounds;

pub(crate) const ENQUIRY_RESPONSE: &str = "seoul";
pub(crate) const XTVERSION_RESPONSE: &str = concat!("seoul ", env!("CARGO_PKG_VERSION"));

#[derive(Debug)]
pub(crate) struct TerminalEffectState {
    pub title: String,
    pub bell_count: u64,
    pub suppress_side_effects: bool,
    pub size: SizeReportSize,
}

impl TerminalEffectState {
    pub fn new(cols: u16, rows: u16, cell_width: f32, line_height: f32) -> Self {
        let mut state = Self {
            title: String::new(),
            bell_count: 0,
            suppress_side_effects: false,
            size: SizeReportSize {
                rows,
                columns: cols,
                cell_width: cell_width.round().max(1.0) as u32,
                cell_height: line_height.round().max(1.0) as u32,
            },
        };
        state.set_size(TerminalBounds {
            cols,
            rows,
            cell_width,
            line_height,
        });
        state
    }

    pub fn set_size(&mut self, bounds: TerminalBounds) {
        self.size = SizeReportSize {
            rows: bounds.rows,
            columns: bounds.cols,
            cell_width: bounds.cell_width.round().max(1.0) as u32,
            cell_height: bounds.line_height.round().max(1.0) as u32,
        };
    }
}

pub(crate) fn device_attributes() -> DeviceAttributes {
    DeviceAttributes {
        primary: PrimaryDeviceAttributes::new(
            ConformanceLevel::VT420,
            [
                DeviceAttributeFeature::SELECTIVE_ERASE,
                DeviceAttributeFeature::ANSI_COLOR,
            ],
        ),
        secondary: SecondaryDeviceAttributes {
            device_type: DeviceType::VT420,
            firmware_version: 1,
            rom_cartridge: 0,
        },
        tertiary: TertiaryDeviceAttributes { unit_id: 0 },
    }
}

pub(crate) fn color_scheme() -> ColorScheme {
    ColorScheme::Dark
}
