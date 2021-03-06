# Changelog

(see http://keepachangelog.com http://semver.org)
(critical comment about semver: https://gist.github.com/jashkenas/cbd2b088e20279ae2c8e)

## Unreleased

## [0.4.2] - 2019-10-22
- Update Rusoto to version 0.41.0

## [0.4.1] - 2019-10-09
- Use `futures::stream::FuturesOrdered` to maintain the order of the merge PDFs.

## [0.4.0] - 2019-08-21
- Use `async-await`
- Replace `tide` with `warp`
- Save merge assets with V4 UUIDs as file names instead of using the original file names to avoid errors when users upload files with the same name.
- Reject merge requests with an empty list of documents to merge
- Bug fixes and cleanup

## [0.3.0] - 2018-05-11
- Add Sentry for error tracking
- Make `papers local` exit with same code as the latex process
- Send better error messages to callback endpoint
- Implement LaTeX escaping
- Force images in merges to have A4 aspect ratio *and* size
- Register `escape_tex` and `unescape_tex` filters for Tera templates
- Report both stdout and stderr in merge errors
- Trim URIs parsed for DocumentSpecs
- Merge local and server binaries

## [0.2.3] - 2017-08-31
- Add a `/merge` endpoint that takes a MergeSpec and merges documents into a
  single PDF file using `pdfunite` and ImageMagick. This is needed for merging
  PDF documents that contain forms (merging can otherwise be done simply with
  latex and `pdfpages`). In the current state, it only preserves the forms on
  the first document being merged due to limitations with available CLI tools.

## [0.2.2] - 2017-08-21
- Properly set the Content-Type header when calling the callback URL.
- Set the Content-Length header instead of Transfer-Encoding: chunked header
  when calling the callback URL. Streaming HTTP is not properly supported by
  some HTTP servers, including Puma (Rails).

## [0.2.1] - 2017-08-18
- Actually upload the workspace.tar
- Set the Content-Type header when posting to callback url
- Fix terminal output in k8s

## [0.2.0] - 2017-08-17
- Major change in the interface of the service. Papers now uploads the rendered
  PDF and the build artifact directly to an S3 bucket, which requires
  additional configuration on your part. The `callback_url` will now receive a
  `Summary`, which contains either an `error` or a `file` field with the
  presigned url to the rendered PDF, and an `s3_folder` field for the location
  of the debugging output.
- Rename all templates and merge from [name].tex to [name].tex.tera
- Better error reporting

## [0.1.0] - 2017-05-23
- Initial release
