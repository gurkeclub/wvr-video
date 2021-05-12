use std::io::Write;

use anyhow::{Context, Result};

use gst::{self, Format, Fraction};
use gst::{prelude::*, Buffer};
use gst::{Element, ElementFactory, Pipeline, State};
use gst_app::{self, AppSrc};
use gst_video::{self, VideoFormat, VideoInfo};

pub enum TextureFormat {
    RGBU8,
    RGBAU8,
    BGRU8,
    BGRAU8,
}

pub struct VideoEncoder {
    pipeline: Pipeline,
    app_src: AppSrc,
}

impl VideoEncoder {
    pub fn new(path: &str, width: usize, height: usize, framerate: f64) -> Result<Self> {
        gst::init().expect("Failed to initialize the gstreamer library");
        let path = if cfg!(target_os = "windows") {
            path.replace('\\', "/")
        } else {
            path.to_owned()
        };

        let pipeline = Pipeline::new(None);

        let appsrc = ElementFactory::make("appsrc", None).unwrap();

        let videoconvert = ElementFactory::make("videoconvert", None).unwrap();

        let videoflip = ElementFactory::make("videoflip", None).unwrap();
        videoflip.set_property("method", &"vertical-flip").unwrap();

        let queue = ElementFactory::make("queue", None).unwrap();

        let x264enc = ElementFactory::make("x264enc", None).unwrap();
        x264enc.set_property("qp-max", &0u32).unwrap();

        let avimux = ElementFactory::make("avimux", None).unwrap();
        let sink = ElementFactory::make("filesink", None).unwrap();
        sink.set_property("location", &path).unwrap();

        pipeline
            .add_many(&[&appsrc, &videoconvert, &queue, &x264enc, &avimux, &sink])
            .unwrap();
        Element::link_many(&[&appsrc, &videoconvert, &queue, &x264enc, &avimux, &sink]).unwrap();

        let appsrc = appsrc.dynamic_cast::<AppSrc>().unwrap();
        let info = VideoInfo::builder(VideoFormat::Rgb, width as u32, height as u32)
            .fps(Fraction::new((framerate * 1000.0) as i32, 1000))
            .build()
            .unwrap();
        appsrc.set_caps(Some(&info.to_caps().unwrap()));
        appsrc.set_property_format(Format::Time);

        pipeline.set_state(State::Playing).context(format!(
            "Failed to start gstreamer encoder for output {:?}",
            path
        ))?;

        Ok(Self {
            pipeline,
            app_src: appsrc,
        })
    }

    pub fn stop(&mut self) {
        if let Err(e) = self.pipeline.set_state(State::Null) {
            eprintln!("Failed to stop video encoding: {:?}", e);
        }
        if let Err(e) = self.app_src.end_of_stream() {
            eprintln!("Failed to end stream: {:?}", e);
        }
    }

    pub fn encode_frame(&mut self, time: f64, frame: &[u8]) {
        let pts = (time * 1_000_000.0) as u64 * gst::NSECOND;
        let mut buffer = Buffer::with_size(frame.len()).unwrap();
        {
            let buffer = buffer.get_mut().unwrap();
            buffer.set_pts(pts);

            let mut data = buffer.map_writable().unwrap();
            let mut data = data.as_mut_slice();

            data.write_all(frame).unwrap();
        }

        if let Err(error) = self.app_src.push_buffer(buffer) {
            panic!("{:?}", error);
        }
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        self.stop();
    }
}
