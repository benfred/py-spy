use std::collections::HashMap;
use std::thread;
use std::time::Instant;
use log::info;
use failure::{Error, ResultExt, format_err};

use mime_guess::guess_mime_type;
use rouille::{Response, Request, Server, router, try_or_400};
use serde_json::json;
use rust_embed::RustEmbed;

mod data_collector;
mod frame_node;
pub use self::data_collector::{TraceCollector, Data};

pub fn start_server(address: &str, collector: &TraceCollector) -> Result<std::net::SocketAddr, Error> {
    let data = collector.data.clone();
    let server = Server::new(address, move |request| http_handler(&data.lock().unwrap(), request))
        .map_err(Error::from_boxed_compat)
        .context("Failed to create web server")?;

    let addr = server.server_addr();
    thread::spawn(move || {
        server.run();
    });
    Ok(addr)
}

/// Routes an http request to the appropiate location
fn http_handler(data: &Data, request: &Request) -> Response {
    let start = Instant::now();
    let response = router!(request,
        // Static assets
        (GET) (/assets/{filename: String}) => { embedded_response(Asset::get(&filename), &filename) },
        (GET) (/js/{filename: String}) => { embedded_response(JavascriptBundle::get(&filename), &filename) },

        // JSON api
        (GET) (/api/stats) => { Response::json(&data.stats) },
        (GET) (/api/aggregated_traces) => {
            let aggregates = get_aggregates(data, request);
            Response::json(&try_or_400!(aggregates.map_err(|x| x.compat())))
        },
        (GET) (/api/flattened_traces) => {
            let aggregates = try_or_400!(get_aggregates(data, request).map_err(|x| x.compat()));
            let flattened = aggregates.flatten();
            let mut flattened_values: Vec<&frame_node::FrameInfo> = flattened.values().collect();

            // filter down to file/function as required
            if let Some(file) = request.get_param("file") {
                match request.get_param("function") {
                    Some(function) => flattened_values.retain(|&row|row.frame.short_filename.as_ref() == Some(&file) && row.frame.name == function),
                    None => flattened_values.retain(|&row| row.frame.short_filename.as_ref() == Some(&file))
                };
            }

            Response::json(&flattened_values)
        },
        (GET) (/api/function_info) => {
            let aggregates = try_or_400!(get_aggregates(data, request).map_err(|x| x.compat()));
            let flattened = aggregates.flatten();
            let mut flattened_values: Vec<&frame_node::FrameInfo> = flattened.values().collect();

            let filename = match request.get_param("file") {
                Some(filename) => filename,
                None => { return Response::text("Must specify a filename param").with_status_code(400); }
            };
            let function = match request.get_param("function") {
                Some(function) => function,
                None => { return Response::text("Must specify a function param").with_status_code(400); }
            };
            let full_filename = match data.short_filenames.get(&filename) {
                Some(filename) => filename.clone(),
                None => { return Response::text("Unknown file").with_status_code(400); }
            };

            flattened_values.retain(|&row| row.frame.short_filename.as_ref() == Some(&filename) && row.frame.name == function);

            let contents = match std::fs::read_to_string(&full_filename) {
                Ok(contents) => contents,
                Err(e) => {
                    return Response::json(&json!({"error": format!("Failed to open file '{}': {}", full_filename, e)}));
                }
            };
            let output = json!({"contents": contents, "flattened": flattened_values});
            Response::json(&output)
        },
        // HTML pages
        (GET) (/) => { try_or_400!(render_template("index", &HashMap::new()).map_err(|x| x.compat())) },
        (GET) (/packages) => { try_or_400!(render_template("packages", &HashMap::new()).map_err(|x| x.compat())) },
        (GET) (/files) => { try_or_400!(render_template("files", &HashMap::new()).map_err(|x| x.compat())) },
        (GET) (/functions) => { try_or_400!(render_template("functions", &HashMap::new()).map_err(|x| x.compat())) },
        (GET) (/function/{name: String}) => {
            let filename = match request.get_param("f") {
                Some(filename) => filename,
                None => { return Response::text("Must specify a filename param").with_status_code(400); }
            };

            let full_filename = match data.short_filenames.get(&filename) {
                Some(filename) => filename.clone(),
                None => { return Response::text("Unknown file").with_status_code(400); }
            };

            let mut template_params = HashMap::new();
            template_params.insert("name".to_owned(), name);
            template_params.insert("short_filename".to_owned(), filename);
            template_params.insert("filename".to_owned(), full_filename);
            let html = render_template("function", &template_params);
            try_or_400!(html.map_err(|x| x.compat()))
        },
        _ =>  { get_404() }
    );

    info!("{} - {} '{}' from {} took {:.2?}", response.status_code, request.method(), request.raw_url(), request.remote_addr(), Instant::now() - start);
    response
}

// we're using rustembed crate to compile everything in the assets folder into the binary
#[derive(RustEmbed)]
#[folder = "src/web_viewer/assets/"]
struct Asset;

#[derive(RustEmbed)]
#[folder = "src/web_viewer/visualizations/dist/$PROFILE"]
struct JavascriptBundle;

fn render_template(template_name: &str, template_args: &HashMap<String, String>) -> Result<Response, Error> {
    let mut handlebars = handlebars::Handlebars::new();
    register_template(&mut handlebars, "base")?;
    register_template(&mut handlebars, template_name)?;
    Ok(Response::from_data("text/html", handlebars.render(template_name, &template_args)?))
}

fn register_template(handlebars: &mut handlebars::Handlebars, name: &str) -> Result<(), Error> {
    let template = match Asset::get(&format!("templates/{}.html", name)) {
        Some(txt) => txt,
        None => { return Err(format_err!("Failed to find template {}", name)); }
    };

    handlebars.register_template_string(name, std::str::from_utf8(&template)?)?;
    Ok(())
}

fn get_aggregates(data: &Data, request: &Request) -> Result<frame_node::FrameNode, Error> {
    let start_time = match request.get_param("start") {
        Some(start) => start.parse()?,
        None => 0_u64
    };

    let end_time = match request.get_param("end") {
        Some(end) => end.parse()?,
        None => 0_u64
    };

    let include_lines = request.get_param("include_lines").is_some();
    let include_threads = request.get_param("include_threads").is_some();
    let include_processes = request.get_param("include_processes").is_some();
    let frame_filter = request.get_param("include_frames");
    let (include_idle, gil_only) = match frame_filter.as_ref().map(String::as_str) {
        Some("idle") => (true, false),
        Some("active") => (false, false),
        Some("gil") => (false, true),
        _ => (false, false)
    };

    let options = frame_node::AggregateOptions{include_lines, include_threads, include_processes, include_idle, gil_only};
    data.aggregate(start_time, end_time, &options)
}

// Given a filename (from the assets folder), returns a rouille response with the file
// (or a 404 if it doesn't exist)
fn embedded_response(embedded: Option<std::borrow::Cow<'static, [u8]>>, filename: &str) -> Response {
    match embedded {
        Some(content) => Response::from_data(guess_mime_type(filename).to_string(), content),
        None => get_404()
    }
}

fn get_404() -> Response {
    match Asset::get("404.html") {
        Some(content) => Response::from_data("text/html", content),
        None => Response::html("404 - not found.")
    }.with_status_code(404)
}