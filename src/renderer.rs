use futures::sync::oneshot;
use futures::future;
use futures::Future;
use futures_cpupool::CpuPool;
use hyper;
use hyper::{Request, Response, Uri};
use hyper::server::Service;
use mktemp::Temp;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio_core::reactor::Handle;
use tokio_process::CommandExt;
use tera::Tera;
use slog::{Drain, Duplicate, Logger};
use sloggers::Build;
use sloggers::types::Severity;
use sloggers::file::FileLoggerBuilder;

use http::*;
use papers::{DocumentSpec, PapersUri};
use error::{Error, ErrorKind};
use config::Config;
use utils::s3::*;
use utils::callbacks::*;

#[derive(Debug)]
pub struct Renderer<S>
where
    S: Service<Request = Request, Response = Response, Error = hyper::Error> + Clone + 'static,
{
    config: &'static Config,
    handle: Handle,
    client: S,
}

impl<S> Renderer<S>
where
    S: Service<Request = Request, Response = Response, Error = hyper::Error> + Clone + 'static,
{
    fn get_template(
        &self,
        template_url: &Uri,
    ) -> Box<Future<Item = hyper::client::Response, Error = Error>> {
        self.client.clone().get_follow_redirect(template_url)
    }

    pub fn new(config: &'static Config, handle: &Handle, client: S) -> Self {
        Renderer {
            client,
            config,
            handle: handle.clone(),
        }
    }

    pub fn preview(
        &self,
        document_spec: DocumentSpec,
        sender: oneshot::Sender<Result<String, Error>>,
    ) -> Box<Future<Item = (), Error = ()>> {
        let DocumentSpec {
            variables,
            template_url,
            ..
        } = document_spec;
        let response = self.get_template(&template_url.0);
        let max_asset_size = self.config.max_asset_size;
        let bytes = response.and_then(move |res| res.get_body_bytes_with_limit(max_asset_size));
        let template_string = bytes.and_then(|bytes| {
            ::std::string::String::from_utf8(bytes).map_err(Error::from)
        });
        let rendered = template_string.and_then(move |template_string| {
            Tera::one_off(&template_string, &variables, false).map_err(Error::from)
        });
        let work = rendered
            .then(|rendered| sender.send(rendered))
            .map(|_| ())
            .map_err(|_| ());
        Box::new(work)
    }

    // Since `mktemp::Temp` implements Drop by deleting the directory, we don't need to worry about
    // leaving files or directories behind. On the flipside, we must ensure it is not dropped before
    // the last returned future that needs the directory finishes.
    pub fn render(&self, document_spec: DocumentSpec) -> Box<Future<Item = (), Error = ()>> {
        let dir = Temp::new_dir();
        let pool = CpuPool::new(3);
        let s3_prefix = s3_dir_name();

        if let Err(err) = dir {
            error!(self.config.logger, "{}", err);
            return Box::new(future::err(()));
        }

        let dir = dir.unwrap();

        let temp_dir_path = dir.to_path_buf();
        let logger = make_file_logger(self.config.logger.clone(), &temp_dir_path);

        let mut template_path = temp_dir_path.clone();
        template_path.push(Path::new(
            &document_spec.output_filename.replace("pdf", "tex"),
        ));
        let max_asset_size = self.config.max_asset_size;

        debug!(
            logger,
            "Trying to generate PDF with document spec: {:?}",
            document_spec
        );

        let DocumentSpec {
            assets_urls,
            callback_url,
            output_filename,
            template_url,
            variables,
            ..
        } = document_spec;

        let res = self.get_template(&template_url.0);

        debug!(logger, "Starting Renderer worker");

        // First download the template and populate it
        let bytes = res.and_then(move |res| res.get_body_bytes_with_limit(max_asset_size));

        let template_string = {
            let logger = logger.clone();
            bytes.and_then(move |bytes| {
                debug!(logger, "Successfully downloaded the template");
                String::from_utf8(bytes).map_err(Error::from)
            })
        };

        let rendered_template = template_string.and_then(move |template_string| {
            Tera::one_off(&template_string, &variables, false).map_err(Error::from)
        });

        let written_template_path = {
            let logger = logger.clone();
            let template_path = template_path.clone();
            rendered_template.and_then(move |latex_string| {
                debug!(logger, "Writing template to {:?}", &template_path);
                let mut file = ::std::fs::File::create(&template_path)?;
                file.write_all(latex_string.as_bytes())
                    .expect("could not write latex file");
                debug!(
                    logger,
                    "Template successfully written to {:?}",
                    &template_path
                );
                Ok(template_path)
            })
        };

        // Download the assets and save them in the temporary directory
        let files_written = {
            let config = self.config.clone();
            let logger = logger.clone();
            let temp_dir_path = temp_dir_path.clone();
            let client = self.client.clone();
            written_template_path.and_then(move |_| {
                download_assets(config, logger, temp_dir_path, client, assets_urls)
            })
        };

        // Then run latex
        let latex_out = {
            let handle = self.handle.clone();
            let template_path = template_path.clone();
            let temp_dir_path = temp_dir_path.clone();
            let logger = logger.clone();
            files_written.and_then(move |_| {
                debug!(logger, "Spawning latex");
                debug!(logger, "template_path {:?}", template_path);
                debug!(logger, "temp_dir_path {:?}", temp_dir_path);
                debug!(
                    logger,
                    "Rendered template exists: {:?}",
                    template_path.exists()
                );
                Command::new("xelatex")
                    .current_dir(&temp_dir_path)
                    .arg("-interaction=nonstopmode")
                    .arg("-file-line-error")
                    .arg("-shell-restricted")
                    .arg(template_path)
                    .output_async(&handle)
                    .map_err(|err| Error::with_chain(err, "Error generating PDF"))
            })
        };

        let output_path = {
            let logger = logger.clone();
            let temp_dir_path = temp_dir_path.clone();
            let output_filename = output_filename.clone();
            latex_out
                .and_then(move |output| {
                    let stdout = String::from_utf8(output.stdout).unwrap();
                    if output.status.success() {
                        debug!(logger, "{}", stdout);
                        Ok(())
                    } else {
                        Err(ErrorKind::LatexFailed(stdout).into())
                    }
                })
                .map(move |_| {
                    // Construct the path to the generated PDF
                    let mut path = temp_dir_path;
                    path.push(Path::new(&output_filename));
                    path
                })
        };

        let s3_upload = {
            let config = self.config.clone();
            let logger = logger.clone();
            let pool = pool.clone();
            let key = format!("{}/{}", &s3_prefix, &output_filename);
            output_path.and_then(move |path| {
                pool.spawn_fn(move || {
                    debug!(
                        logger,
                        "Uploading the rendered pdf as {:?} / {:?}",
                        config.s3.bucket,
                        key
                    );
                    post_to_s3(config, &path, key.clone())?;
                    get_presigned_url(config, key)
                })
            })
        };

        let callback_response = {
            let callback_url = callback_url.clone();
            let client = self.client.clone();
            let config = self.config.clone();
            let logger = logger.clone();
            let s3_prefix = s3_prefix.clone();
            s3_upload.and_then(move |presigned_url| {
                report_success(
                    config,
                    logger,
                    client,
                    callback_url.0,
                    s3_prefix,
                    presigned_url,
                )
            })
        };

        // Report errors to the callback url
        let handle_errors = {
            let logger = logger.clone();
            let client = self.client.clone();
            let s3_prefix = s3_prefix.clone();
            callback_response.or_else(move |error| {
                report_failure(logger, client, error, s3_prefix, callback_url.0)
            })
        };

        let tarred_workspace_uploaded = {
            let config = self.config.clone();
            let key = format!("{}/{}", &s3_prefix, "workspace.tar");
            let temp_dir_path = temp_dir_path.clone();
            let logger = logger.clone();
            handle_errors
                .then(move |_| {
                    pool.spawn_fn(move || upload_workspace(config, logger, temp_dir_path, key))
                })
                .map_err(move |_| { let _hold = dir; })
        };


        Box::new(tarred_workspace_uploaded)
    }
}

/// This returns a logger that also logs to the file pointed by the path parameter on top of the
/// provided logger. The returned logger logs to both outputs.
///
/// The file logger has the debug level since this is what we want for debugging.
fn make_file_logger(logger: Logger, path: &Path) -> Logger {
    let mut dest = path.to_path_buf();
    dest.push("logs.txt");
    let file_drain = FileLoggerBuilder::new(dest)
        .level(Severity::Debug)
        .build()
        .expect("Could not create a file logger");
    let drain = Duplicate::new(logger.clone(), file_drain).fuse();
    Logger::root(drain, o!())
}

/// Downloads all assets from the document spec in the workspace in parallel. It fails if any of
/// those cannot be downloaded.
fn download_assets<S>(
    config: &'static Config,
    logger: Logger,
    temp_dir_path: PathBuf,
    client: S,
    assets_urls: Vec<PapersUri>,
) -> Box<Future<Item = Vec<()>, Error = Error>>
where
    S: Service<Request = Request, Response = Response, Error = hyper::Error> + 'static + Clone,
{
    let max_asset_size = config.max_asset_size.clone();
    debug!(logger, "Downloading assets {:?}", assets_urls);
    let futures = assets_urls.into_iter().map(move |uri| {
        let logger = logger.clone();
        let mut path = temp_dir_path.to_path_buf();
        let client = client.clone();

        let response = client.get_follow_redirect(&uri.0);

        let body = response.and_then(move |res| {
            let filename = res.filename();
            res.get_body_bytes_with_limit(max_asset_size)
                .map(|bytes| (bytes, filename))
        });
        body.and_then(move |(bytes, filename)| {
            let filename = filename.or_else(|| extract_filename_from_uri(&uri.0));
            match filename {
                Some(filename) => {
                    path.push(filename);
                    debug!(logger, "Writing asset {:?} as {:?}", uri, path);
                    ::std::fs::File::create(&path)
                        .and_then(|mut file| file.write_all(&bytes))
                        .map_err(|e| Error::with_chain(e, "Error writing asset"))
                }
                _ => Ok(()),
            }
        })
    });
    Box::new(future::join_all(futures))
}
