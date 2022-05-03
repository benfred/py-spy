import {json} from "d3-fetch";
import {interval, Timer} from "d3-timer";
import {select, selectAll} from "d3-selection";
import {Flamegraph} from "./flamegraph";
import {FunctionTable} from "./function_table";
import {TimeSeriesSelector} from "./time_series_selector";
import {display_error_message} from "./utils";

export {CodeDetails} from "./code_details";

abstract class TimeSeriesOverview {
    public data: any = null;
    public times: TimeSeriesSelector;
    public timer: Timer;

    constructor(public timescale_element: HTMLElement) {
        this.times = new TimeSeriesSelector(this.timescale_element);

        this.times.load = (start: number, end: number) => { this.load_data(start, end); }

        // update form inputs to match url params
        let search = new URLSearchParams(window.location.search);
        for (let name of ["include_threads", "include_lines", "include_processes"]) {
            let element = document.getElementById(name) as HTMLInputElement;
            if (element !== null) {
                element.checked = search.has(name);
            }
        }
        if (search.has("include_frames")) {
            let select = document.getElementById("include_frames") as HTMLSelectElement;
            select.value = search.get("include_frames");
        }

        let times = this.times;
        selectAll("input[type=checkbox].flameoption").on("change", function() {
            let search = new URLSearchParams(window.location.search);
            let checkbox = this as HTMLInputElement;
            if (checkbox.checked) {
                search.set(checkbox.id, "1");
            } else {
                search.delete(checkbox.id);
            }
            history.replaceState({}, "", "?" + search.toString());
            times.load(times.selected[0], times.selected[1]);
        });

        selectAll("select.flameoption").on("change", function() {
            let search = new URLSearchParams(window.location.search);
            let select = this as HTMLInputElement;
            search.set(select.id, select.value);
            history.pushState({}, "", "?" + search.toString());
            times.load(times.selected[0], times.selected[1]);
        });

        this.load_stats();
        this.timer = interval(() => this.load_stats(), 1000);
    }

    public display_stats(d: any): void {
        document.getElementById("python_version").textContent = d.version;
        document.getElementById("sampling_rate").textContent = d.sampling_rate;

        if (d.python_command.length) {
            document.getElementById("python_command").textContent = d.python_command;
        }

        // Get the gil/thread activity in a format we want
        let active = d.threads[0][1];
        for (let [thread, values] of d.threads.slice(1)) {
            for (let i  = 0; i < values.length; ++i) {
                active[i] += values[i];
            }
        }
        let max_active = Math.ceil(Math.max.apply(null, active) - .4);
        let active_name = '% Active';
        if (max_active > 1) {
            for (let i = 0; i < active.length; ++i) {
                active[i] /= max_active;
            }
            active_name = active_name + " (out of " + max_active  + " threads)";
        }

        let data = [{name: active_name, values: active, legend_x: 50, colour: "#1f77b4" },
                     {name: '% GIL', values: d.gil, legend_x: 0, colour: "#ff7f0e"}];
        this.times.update(data);
    }

    public load_stats(): void {
        json("/api/stats")
            .then((d: any) => {
                if (!d.running) {
                    this.timer.stop();
                    document.getElementById("runningstate").textContent = "stopped";
                }

                if (d.sampling_delay && d.sampling_delay.secs >= 1) {
                    display_error_message(`${d.sampling_delay.secs}s behind in sampling, results may be inaccurate. Try reducing the sampling rate`);
                }
                if (d.subprocesses) {
                    selectAll(".subprocess_option").classed("hidden", false);
                }
                this.display_stats(d);
            })
            .catch(err => {
                display_error_message(err);
                console.log(err);
                throw(err);
            });
    }

    public load_data(start: number, end: number): void {
        let url = this.get_url_base() + "?start=" + Math.floor(start * 1000) + "&end=" + Math.floor(end * 1000);
        if (window.location.search.length > 0) {
            url += "&" + window.location.search.slice(1);
        }

        // store a reference to the data (needed to update flamegraph on resize etc)
        // select("#spinner").style("display", "inline-block");
        json(url)
            .then((d: any) => {
                select("#spinner").style("display", "none");
                document.getElementById("startselection").textContent = start.toFixed(3) + "s";
                document.getElementById("endselection").textContent = end.toFixed(3) + "s";
                // TODO: document.getElementById("countselection").textContent = d.value.toLocaleString();
                this.display_data(d, this.data != null);

                // store reference so that we can redraw easily on resize
                this.data = d;
            })
            .catch(err => {
                select("#spinner").style("display", "none");
                display_error_message(err);
                console.log("Failed to get", url, err);
            });
    }

    abstract display_data(data: any, transition: boolean): void;

    abstract get_url_base(): string;
}

export class FlamegraphOverview extends TimeSeriesOverview {
    public flamegraph: Flamegraph;

    constructor(public flame_element: HTMLElement,
                public timescale_element: HTMLElement) {
        super(timescale_element);

        let div = select(flame_element);
        this.flame_element = div.nodes()[0] as HTMLElement;
        this.flamegraph = new Flamegraph();

        // handle resizes somewhat gracefully
        window.addEventListener("resize", () => {
            this.times.resize();
            if (this.data !== null) {
                this.flamegraph.update(this.flame_element, this.data, true);
            }
        });
    }

    public display_stats(d: any): void {
        this.flamegraph.sampling_rate = d.sampling_rate;
        super.display_stats(d);
    }

    public display_data(data: any, transition: boolean): void {
        document.getElementById("countselection").textContent = data.total.toLocaleString();
        let countdetails = get_frame_descriptions(data);
        if (countdetails.length) {
            document.getElementById("countdetails").innerHTML = "<br>" + countdetails + ".";
        }

        this.flamegraph.update(this.flame_element, data.root, transition);
    }

    public get_url_base(): string {
        return "/api/aggregated_traces";
    }
}

export class FunctionTableOverview extends TimeSeriesOverview {
    public table: FunctionTable;

    constructor(public table_element: HTMLElement,
                public timescale_element: HTMLElement) {
        super(timescale_element);
        this.table = new FunctionTable();
        window.addEventListener("resize", () => this.times.resize());
    }

    public display_data(data: any, transition: boolean): void {
        document.getElementById("countselection").textContent = data.total.toLocaleString();
        let countdetails = get_frame_descriptions(data);
        if (countdetails.length) {
            document.getElementById("countdetails").innerHTML = "<br>" + countdetails + ".";
        }
        // remove the 'all' row'
        let flattened = data.flattened.filter((row:any) => row.frame.short_filename);
        this.table.update(this.table_element, flattened, data.total);
    }

    public get_url_base(): string {
        return "/api/flattened_traces";
    }
}

function get_frame_descriptions(data: any): string {
    let parts = []
    if (data.total != data.active) {
        parts.push(`<b>${(100.0 * data.active / data.total).toLocaleString()}</b>% Active`);
    }
    if (data.total != data.gil) {
        parts.push(`<b>${(100.0 * data.gil / data.total).toLocaleString()}</b>% with GIL`);
    }
    if (data.filtered > 0) {
        parts.push(`<b>${data.filtered}</b> stack traces not shown`);
    }
    return parts.join(", ");
}
