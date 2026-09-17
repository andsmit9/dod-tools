use super::*;

impl Doer for SvcCdTrack {
    fn id(&self) -> u8 {
        32
    }

    fn parse(i: &[u8], _: AuxRefCell) -> Result<Self> {
        map((le_i8, le_i8), |(track, loop_track)| Self {
            track,
            loop_track,
        }).parse(i)
    }

    fn write(&self, _: AuxRefCell) -> ByteVec {
        let mut writer = ByteWriter::new();

        writer.append_u8(self.id());

        writer.append_i8(self.track);
        writer.append_i8(self.loop_track);

        writer.data
    }
}
