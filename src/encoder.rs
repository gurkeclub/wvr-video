use std::io::Write;

use anyhow::{Context, Result};

use gst;
use gst::prelude::*;
use gst::State;
use gst_app;
use gst_video;

pub enum TextureFormat {
    RGBU8,
    RGBAU8,
    BGRU8,
    BGRAU8,
}

pub struct VideoEncoder {
    pipeline: gst::Pipeline,
    app_src: gst_app::AppSrc,
}

impl VideoEncoder {
    pub fn new(path: &str, width: usize, height: usize, framerate: f64) -> Result<Self> {
        gst::init().expect("Failed to initialize the gstreamer library");
        let path = if cfg!(target_os = "windows") {
            path.replace('\\', "/")
        } else {
            path.to_owned()
        };

        let pipeline = gst::Pipeline::new(None);
        let appsrc = gst::ElementFactory::make("appsrc", None).unwrap();
        let videoconvert = gst::ElementFactory::make("videoconvert", None).unwrap();
        let queue = gst::ElementFactory::make("queue", None).unwrap();

        let avimux = gst::ElementFactory::make("avimux", None).unwrap();
        let sink = gst::ElementFactory::make("filesink", None).unwrap();
        sink.set_property("location", &path).unwrap();

        pipeline
            .add_many(&[&appsrc, &videoconvert, &queue, &avimux, &sink])
            .unwrap();
        gst::Element::link_many(&[&appsrc, &videoconvert, &queue, &avimux, &sink]).unwrap();

        let appsrc = appsrc.dynamic_cast::<gst_app::AppSrc>().unwrap();
        let info =
            gst_video::VideoInfo::builder(gst_video::VideoFormat::Rgb, width as u32, height as u32)
                .fps(gst::Fraction::new((framerate * 1000.0) as i32, 1000))
                .build()
                .unwrap();
        appsrc.set_caps(Some(&info.to_caps().unwrap()));
        appsrc.set_property_format(gst::Format::Time);

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
        if let Err(e) = self.pipeline.set_state(State::Paused) {
            eprintln!("Failed to stop video encoding: {:?}", e);
        }
        if let Err(e) = self.app_src.end_of_stream() {
            eprintln!("Failed to end stream: {:?}", e);
        }
        println!("Pipeline stopped");
    }

    pub fn encode_frame(&mut self, time: f64, frame: &[u8]) {
        let mut buffer = gst::Buffer::with_size(frame.len()).unwrap();
        {
            let buffer = buffer.get_mut().unwrap();
            buffer.set_pts((time * 1000.0) as u64 * gst::MSECOND);

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
