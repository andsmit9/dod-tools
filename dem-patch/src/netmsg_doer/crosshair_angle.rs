use super::*;

impl Doer for SvcCrosshairAngle {
    fn id(&self) -> u8 {
        47
    }

    fn parse(i: &[u8], _: AuxRefCell) -> Result<Self> {
        map((le_i16, le_i16), |(pitch, yaw)| Self { pitch, yaw }).parse(i)
    }

    fn write(&self, _: AuxRefCell) -> ByteVec {
        let mut writer = ByteWriter::new();

        writer.append_u8(self.id());

        writer.append_i16(self.pitch);
        writer.append_i16(self.yaw);

        writer.data
    }
}
