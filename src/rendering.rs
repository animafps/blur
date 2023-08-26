use crate::config::Config;
use crate::helpers::{self, change_file_name, clean, exec};
use crate::script_handler::create;
use crate::teres::{create_temp_path, used_installer};
use crate::vapoursynth::output::output;
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, error};
use rustsynth::core::CoreCreationFlags;
use rustsynth::core::CoreRef;
use rustsynth::node::Node;
use rustsynth::vsscript::Environment;
use rustsynth_derive::init_plugins;
use std::path::{Path, PathBuf};
use std::vec::Vec;

#[derive(Clone)]
pub struct Render {
    pub video_path: PathBuf,
    pub video_folder: PathBuf,
    pub script_path: PathBuf,

    input_filename: String,
    output_filepath: PathBuf,

    settings: Config,
    pub stdout: bool,
}

impl Render {
    pub fn new(input_path: PathBuf, stdout: bool) -> Option<Render> {
        let video_folder = input_path.parent()?.to_path_buf();
        let video_path = input_path;

        let video_name = video_path.file_stem()?.to_str()?.to_string();

        let input_filename = video_path.file_name()?.to_str()?.to_string();
        let settings = Config::parse();
        let output_filepath = video_folder.join(format!(
            "{}_blur.{}",
            video_name, settings.encoding.container
        ));
        let temp_path = create_temp_path(video_folder.clone()).unwrap();
        let script_path = create(temp_path, &video_path, settings.clone());

        Some(Render {
            video_path,
            video_folder,
            input_filename,
            output_filepath,
            script_path,
            settings,
            stdout,
        })
    }
}

impl PartialEq for Render {
    fn eq(&self, other: &Self) -> bool {
        self.video_path == other.video_path
    }
}

#[derive(Clone)]
pub struct Rendering {
    pub queue: Vec<Render>,
    pub renders_queued: bool,
}

pub struct CommandWithArgs {
    pub ffmpeg_exe: String,
    pub ffmpeg_args: Vec<String>,

    pub vspipe_exe: String,
    pub vspipe_args: Vec<String>,

    pub output_filename: String,
}

impl Rendering {
    pub fn queue_render(&mut self, render: Render) {
        self.queue.push(render);
        self.renders_queued = true;
    }

    pub fn render_videos(&mut self) {
        let core = CoreRef::new(CoreCreationFlags::NONE);

        init_plugins!();

        if self.renders_queued {
            for render in self.queue.iter() {
                eprintln!("Processing {}", render.input_filename);
                let output_filepath = render.output_filepath.clone();
                let settings = render.settings.clone();
                let video_path = render.video_path.clone();
                let script_path = render.script_path.clone();
                let progress = ProgressBar::new(100);
                progress.set_style(
                    ProgressStyle::default_bar()
                        .template(
                            format!(
                                " [{}] {{wide_bar:.cyan/blue}} {{percent}}% {{eta_precise}}",
                                video_path.file_name().unwrap().to_str().unwrap()
                            )
                            .as_str(),
                        )
                        .unwrap(),
                );
                let clip = Plugins::ffms2::Source(&core, video_path.to_str().unwrap().to_owned())
                    .get_node("clip")
                    .unwrap();
                Rendering::render_node(clip, output_filepath, settings, progress, render.stdout)
                    .expect("Render thread failed");
            }
            self.queue.clear();
            self.renders_queued = false;
        }
    }

    pub fn render_video(
        output_filepath: PathBuf,
        settings: Config,
        video_path: PathBuf,
        script_path: PathBuf,
        progress_bar: ProgressBar,
        stdout: bool,
    ) -> Result<(), std::io::Error> {
        let video_clone = video_path.clone();

        let ffmpeg_settings = Rendering::build_ffmpeg_command(
            &script_path,
            &video_clone,
            &output_filepath,
            settings,
            stdout,
        )?;

        debug!(
            "Starting processes with {} {} | {} {}",
            ffmpeg_settings.vspipe_exe,
            ffmpeg_settings.vspipe_args.join(" "),
            ffmpeg_settings.ffmpeg_exe,
            ffmpeg_settings.ffmpeg_args.join(" ")
        );

        let now = std::time::Instant::now();
        let filename = ffmpeg_settings.output_filename.clone();
        let process = exec(ffmpeg_settings, progress_bar);
        if !process.success() {
            error!("Processing failed");
            helpers::exit(exitcode::SOFTWARE);
        }
        eprintln!(
            "Finished processing {} to {} in {}",
            video_path.file_name().unwrap().to_str().unwrap(),
            filename,
            indicatif::HumanDuration(now.elapsed())
        );
        clean(video_clone, script_path);
        Ok(())
    }

    pub fn build_ffmpeg_command(
        script_path: &Path,
        video_path: &Path,
        output_path: &Path,
        settings: Config,
        stdout: bool,
    ) -> Result<CommandWithArgs, std::io::Error> {
        let mut vspipe_path = "vspipe";
        let mut ffmpeg_path = "ffmpeg";
        let vspipe_exe;
        let ffmpeg_exe;

        if used_installer()? {
            let exepath = std::env::current_exe()?;
            let path = exepath.parent().unwrap();
            vspipe_exe = format!("{}/lib/vapoursynth/VSPipe.exe", path.to_str().unwrap());
            vspipe_path = vspipe_exe.as_str();
            ffmpeg_exe = format!("{}/lib/ffmpeg/ffmpeg.exe", path.to_str().unwrap());
            ffmpeg_path = ffmpeg_exe.as_str();
        }

        let pipe_args = vec![
            script_path.to_str().unwrap().to_string(),
            "-".to_string(),
            "-p".to_string(),
            "-c".to_string(),
            "y4m".to_string(),
        ];

        let infile = video_path.display().to_string();

        let mut ffmpeg_command = vec![
            "-loglevel",
            "error",
            "-hide_banner",
            "-nostats",
            "-i",
            "-",
            "-i",
            infile.as_str(),
            "-map",
            "0:v",
            "-map",
            "1:a?",
        ];
        // audio filters
        let mut audio_filters = String::new();
        if settings.timescale.input != 1.0 {
            // asetrate: speed up and change pitch
            audio_filters +=
                format!("asetrate=48000*{}", (1.0 / settings.timescale.input)).as_str();
        }

        if settings.timescale.output != 1.0 {
            if !audio_filters.is_empty() {
                audio_filters += ",";
            }
            if settings.timescale.adjust_audio_pitch {
                audio_filters += format!("asetrate=48000*{}", settings.timescale.output).as_str();
            } else {
                // atempo: speed up without changing pitch
                audio_filters += format!("atempo={}", settings.timescale.output).as_str();
            }
        }

        let formatted_audio;
        if !audio_filters.is_empty() {
            ffmpeg_command.push("-af");
            formatted_audio = audio_filters;
            ffmpeg_command.push(formatted_audio.as_str());
        }

        let quality = &settings.encoding.quality.to_string();
        let custom_ffmpeg = settings.advanced.encoding.custom_ffmpeg_filters;
        if custom_ffmpeg.is_some() {
        } else {
            // video format
            if settings.advanced.encoding.gpu {
                if settings.advanced.encoding.gpu_type.to_lowercase() == "nvidia" {
                    ffmpeg_command.push("-c:v");
                    ffmpeg_command.push("h264_nvenc");
                    ffmpeg_command.push("-preset");
                    ffmpeg_command.push("p7");
                    ffmpeg_command.push("-qp");
                    ffmpeg_command.push(quality);
                } else if settings.advanced.encoding.gpu_type.to_lowercase() == "amd" {
                    ffmpeg_command.push("-c:v");
                    ffmpeg_command.push("h264_amf");
                    ffmpeg_command.push("-qp_i");
                    ffmpeg_command.push(quality);
                    ffmpeg_command.push("-qp_b");
                    ffmpeg_command.push(quality);
                    ffmpeg_command.push("-qp_p");
                    ffmpeg_command.push(quality);
                    ffmpeg_command.push("-quality");
                    ffmpeg_command.push("quality");
                } else if settings.advanced.encoding.gpu_type.to_lowercase() == "intel" {
                    ffmpeg_command.append(&mut vec![
                        "-c:v",
                        "h264_qsv",
                        "-global_quality",
                        quality,
                        "-preset",
                        "veryslow",
                    ]);
                }
            } else if !stdout {
                ffmpeg_command.append(&mut vec![
                    "-c:v",
                    "libx264",
                    "-preset",
                    "superfast",
                    "-crf",
                    quality,
                ]);
            } else {
                ffmpeg_command.append(&mut vec!["-c:v", "rawvideo"])
            }

            // audio format
            ffmpeg_command.append(&mut vec!["-c:a", "aac", "-b:a", "320k"]);

            // extra
            ffmpeg_command.append(&mut vec!["-movflags", "+faststart"]);
        }

        // output
        let outfile = if settings.encoding.detailed_filename
            && settings.interpolation.enabled
            && settings.blending.enabled
        {
            change_file_name(
                output_path,
                format!(
                    "{}-{}fps-{}~{}fps-{}",
                    output_path.file_stem().unwrap().to_str().unwrap(),
                    settings.interpolation.fps,
                    settings.advanced.interpolation.program,
                    settings.blending.output_fps,
                    settings.blending.amount
                )
                .as_str(),
            )
            .display()
            .to_string()
        } else if stdout {
            ffmpeg_command.append(&mut vec!["-f", "nut"]);
            String::from("-")
        } else {
            output_path.display().to_string()
        };
        ffmpeg_command.push(outfile.as_str());
        debug!("{:?}", ffmpeg_command);

        let ffmpeg_args: Vec<String> = ffmpeg_command.iter().map(|n| n.to_string()).collect();
        Ok(CommandWithArgs {
            ffmpeg_exe: ffmpeg_path.to_string(),
            ffmpeg_args,

            vspipe_exe: vspipe_path.to_string(),
            vspipe_args: pipe_args,

            output_filename: outfile,
        })
    }
    pub fn render_node(
        clip: Node,
        output_filepath: PathBuf,
        settings: Config,
        progress_bar: ProgressBar,
        stdout: bool,
    ) -> Result<(), std::io::Error> {
        return Ok(());
    }
}
