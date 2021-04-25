use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use image::DynamicImage;

use gst::prelude::*;
use gst::FlowError;
use gst::State;

use wvr_data::config::project_config::Speed;
use wvr_data::Buffer;
use wvr_data::DataHolder;
use wvr_data::InputProvider;

type BgrImage = image::ImageBuffer<image::Bgr<u8>, Vec<u8>>;
type BgraImage = image::ImageBuffer<image::Bgra<u8>, Vec<u8>>;

pub enum TextureFormat {
    RGBU8,
    RGBAU8,
    BGRU8,
    BGRAU8,
}

pub struct VideoProvider {
    name: String,
    video_buffer: Arc<Mutex<Buffer>>,
    pipeline: gst::Element,

    stop_lock: Arc<AtomicBool>,

    beat: Arc<Mutex<f64>>,
    next_sync_beat: Arc<Mutex<f64>>,

    time: Arc<Mutex<f64>>,
    next_sync_time: Arc<Mutex<f64>>,

    speed: Arc<Mutex<Speed>>,
}

impl VideoProvider {
    pub fn new(path: &str, name: String, resolution: (usize, usize), speed: Speed) -> Result<Self> {
        gst::init().expect("Failed to initialize the gstreamer library");
        let path = if path.starts_with("http") {
            path.to_owned()
        } else {
            let path = if cfg!(target_os = "windows") {
                path.replace('\\', "/")
            } else {
                path.to_owned()
            };

            format!("file:///{}", path)
        };

        let video_buffer = Arc::new(Mutex::new(Buffer {
            dimensions: vec![resolution.0, resolution.1, 3],
            data: None,
        }));


        let speed = Arc::new(Mutex::new(speed));
        
        let stop_lock = Arc::new(AtomicBool::new(false));

        let beat = Arc::new(Mutex::new(0.0));
        let next_sync_beat = Arc::new(Mutex::new(0.0));

        let time = Arc::new(Mutex::new(0.0));
        let next_sync_time = Arc::new(Mutex::new(0.0));

        let pipeline_string = format!(
            "uridecodebin uri={} ! videoconvert ! videoscale ! video/x-raw,format=RGB,format=RGBA,format=BGR,format=BGRA,width={:},height={:} ! videoflip method=vertical-flip ! appsink name=appsink async=false sync=false",
            path, resolution.0, resolution.1,
        );

        let pipeline =
            gst::parse_launch(&pipeline_string).context("Failed to build gstreamer pipeline")?;

        let sink = pipeline
            .clone()
            .dynamic_cast::<gst::Bin>()
            .expect("Failed to cast the gstreamer pipeline as a gst::Bin element")
            .get_by_name("appsink")
            .expect("Failed to retrieve sink from gstreamer pipeline.");

        let appsink = sink
            .dynamic_cast::<gst_app::AppSink>()
            .expect("The sink defined in the pipeline is not an appsink");

        {
            let speed_mutex = speed.clone();
            let stop_lock = stop_lock.clone();

            let beat = beat.clone();
            let next_sync_beat = next_sync_beat.clone();

            let time = time.clone();
            let next_sync_time = next_sync_time.clone();

            let video_buffer = video_buffer.clone();
            appsink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |appsink| {
                        loop {
                            if stop_lock.load(Ordering::Relaxed) {
                                    break;
                                }
                            let speed;
                            if let Ok(speed_mutex) = speed_mutex.lock() {
                                speed = speed_mutex.to_owned();
                            } else {
                                // The main thread most likely crashed
                                return Err(gst::FlowError::Eos);
                            }

                            match speed {
                                Speed::Beats(beat_interval) => {
                                    if let Ok(beat) = beat.lock() {
                                        if let Ok(mut next_sync_beat) = next_sync_beat.lock() {
                                            if *beat > *next_sync_beat {
                                                *next_sync_beat += beat_interval as f64;
                                                break;
                                            }
                                        } else {
                                            // The main thread most likely crashed
                                            return Err(gst::FlowError::Eos);
                                        }
                                    } else {
                                        // The main thread most likely crashed
                                        return Err(gst::FlowError::Eos);
                                    }
                                }
                                Speed::Fps(frame_rate) => {
                                    if let Ok(time) = time.lock() {
                                        if let Ok(mut next_sync_time) = next_sync_time.lock() {
                                            if *time > *next_sync_time {
                                                *next_sync_time += 1.0 / frame_rate as f64;
                                                break;
                                            } 
                                        } else {
                                            // The main thread most likely crashed
                                            return Err(gst::FlowError::Error);
                                        }
                                    } else {
                                        // The main thread most likely crashed
                                        return Err(gst::FlowError::Error);
                                    }
                                }
                            }
                            thread::sleep(Duration::from_micros(50))
                        }
                        

                        let sample = match appsink.pull_sample() {
                            Err(e) => {
                                eprintln!("{:}", e);
                                return Err(gst::FlowError::Eos);
                            }
                            Ok(sample) => sample,
                        };

                        let sample_caps = if let Some(sample_caps) = sample.get_caps() {
                            sample_caps
                        } else {
                            
                            return Err(gst::FlowError::Error);
                        };

                        let video_info = if let Ok(video_info) = gst_video::VideoInfo::from_caps(sample_caps) {
                            video_info
                        } else {
                            
                            return Err(gst::FlowError::Error);
                        };

                        let buffer = if let Some(buffer) = sample.get_buffer() {
                            buffer
                        } else {
                            
                            return Err(gst::FlowError::Error);
                        };

                        let map = if let Ok(map) = buffer.map_readable() {
                            map
                        } else {
                            
                            return Err(gst::FlowError::Error);
                        };

                        let samples = map.as_slice().to_vec();
                        let format = match video_info.format() {
                            gst_video::VideoFormat::Rgb => TextureFormat::RGBU8,
                            gst_video::VideoFormat::Rgba => TextureFormat::RGBAU8,
                            gst_video::VideoFormat::Bgr => TextureFormat::BGRU8,
                            gst_video::VideoFormat::Bgra => TextureFormat::BGRAU8,
                            //gst_video::VideoFormat::Gray16Le => TextureFormat::RF16,
                            unsupported_format => {
                                eprintln!("Unsupported gstreamer format '{:?}'", unsupported_format);
                                return Err(gst::FlowError::Error);
                            }
                        };

                        let image_buffer = match format {
                            TextureFormat::RGBU8 => DynamicImage::ImageRgb8(image::RgbImage::from_raw(video_info.width(), video_info.height(), samples).unwrap()).into_rgb8(),
                            TextureFormat::RGBAU8 => DynamicImage::ImageRgba8(image::RgbaImage::from_raw(video_info.width(), video_info.height(), samples).unwrap()).into_rgb8(),
                            TextureFormat::BGRU8 => DynamicImage::ImageBgr8(BgrImage::from_raw(video_info.width(), video_info.height(), samples).unwrap()).into_rgb8(),
                            TextureFormat::BGRAU8 => DynamicImage::ImageBgra8(BgraImage::from_raw(video_info.width(), video_info.height(), samples).unwrap()).into_rgb8(),
                        };

                        let image_buffer = image_buffer.into_vec();

                        match video_buffer.lock() {
                            Ok(mut video_buffer) => {
                                video_buffer.data = Some(image_buffer);
                                video_buffer.dimensions = vec![video_info.width() as usize, video_info.height() as usize, 3];
                            }
                            Err(e) => {
                                eprintln!("Could not lock video buffer, did the main thread panic? \n{:?}", e);
                                return Err(FlowError::Error);
                            }
                        }


                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }

        pipeline.set_state(State::Playing).context(format!(
            "Failed to start gstreamer pipeline for video {:?}",
            path
        ))?;

        Ok(Self {
            name,
            video_buffer,
            pipeline,
            time,
            stop_lock,
            next_sync_time,
            beat,
            next_sync_beat,
            speed,
        })
    }

    pub fn check_loop(&mut self) {
        if let Some(view) = self
            .pipeline
            .get_bus()
            .expect("Failed to find bus for video playback pipeline")
            .timed_pop(gst::ClockTime::from_seconds(0))
        {
            if let gst::MessageView::Eos(_) = view.view() {
                self.pipeline
                    .seek_simple(
                        gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                        gst::ClockTime::from_seconds(0),
                    )
                    .ok();
            }
        }
    }

}

impl Drop for VideoProvider {
    fn drop(&mut self) {
        self.stop();
    }
}

impl InputProvider for VideoProvider {
    fn set_name(&mut self, name: &str) {
        self.name = name.to_owned();
    }

    fn provides(&self) -> Vec<String> {
        vec![self.name.clone()]
    }
    
    fn set_property(&mut self, property: &str, value: &DataHolder) {
        match (property, value) {
            ("speed_beats", DataHolder::Float(new_speed)) => if let Ok(mut speed) = self.speed.lock() {
                *speed = Speed::Beats(*new_speed);
            }
            ("speed_fps", DataHolder::Float(new_speed)) => if let Ok(mut speed) = self.speed.lock() {
                *speed = Speed::Fps(*new_speed);
            }
            _ => eprintln!("Set_property unimplemented for {:}", property),
        }
    }

    fn get(&mut self, uniform_name: &str, invalidate: bool) -> Option<DataHolder> {
        if uniform_name == self.name {
            self.check_loop();

            if let Ok(mut video_buffer) = self.video_buffer.lock() {
                let result = if let Some(ref data) = video_buffer.data {
                    Some(DataHolder::Texture((
                        (
                            video_buffer.dimensions[0] as u32,
                            video_buffer.dimensions[1] as u32,
                        ),
                        data.to_vec(),
                    )))
                } else {
                    None
                };

                if invalidate {
                    video_buffer.data = None;
                }

                result
            } else {
                None
            }
        } else {
            None
        }
    }

    fn set_beat(&mut self, beat: f64, sync: bool) {
        if let Ok(mut own_beat) = self.beat.lock() {
            // Succesful locking of the Mutex is only checked here as use of the other mutexes depend on this one
            *own_beat = beat;
        } else {
            return;
        }

        if sync {
            let speed;
            if let Ok(speed_mutex) = self.speed.lock() {
                speed = speed_mutex.to_owned();
            } else {
                return;
            }

            if let Speed::Beats(_) = speed {
                let wait_for_sync = if let Ok(next_sync_beat) = self.next_sync_beat.lock() {
                    beat > *next_sync_beat
                } else {
                    // The video reading thread has most probably crashed
                    return;
                };

                if wait_for_sync {
                    loop {
                        if let Ok(next_sync_beat) = self.next_sync_beat.lock() {
                            if beat <= *next_sync_beat {
                                break;
                            }
                        } else {
                            // The video reading thread has most probably crashed
                            return;
                        };
                        self.check_loop();
                    }
                }
            }
        }
    }

    fn set_time(&mut self, time: f64, sync: bool) {
        if let Ok(mut own_time) = self.time.lock() {
            *own_time = time;
        } else {
            // The video reading thread has most probably crashed
            return;
        }

        if sync {
            let speed;
            if let Ok(speed_mutex) = self.speed.lock() {
                speed = speed_mutex.to_owned();
            } else {
                return;
            }

            if let Speed::Fps(_) = speed {
                let wait_for_sync = if let Ok(next_sync_time) = self.next_sync_time.lock() {
                    time > *next_sync_time
                } else {
                    // The video reading thread has most probably crashed
                    return;
                };

                if wait_for_sync {
                    loop {
                        if let Ok(next_sync_time) = self.next_sync_time.lock() {
                            if time <= *next_sync_time {
                                break;
                            }
                        } else {
                            // The video reading thread has most probably crashed
                            return;
                        };
                        self.check_loop();
                    }
                }
            }
        }
    }

    fn stop(&mut self) {
        self.stop_lock.store(true, Ordering::Relaxed);
        
        if let Err(e) = self.pipeline.set_state(State::Null) {
            eprintln!("Failed to stop video playback: {:?}", e);
        }
         

    }
}
 