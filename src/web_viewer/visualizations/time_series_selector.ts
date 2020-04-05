import {event, select} from "d3-selection";
import {brushX, BrushBehavior} from "d3-brush";
import {scaleLinear, ScaleLinear} from "d3-scale";
import {axisBottom} from "d3-axis";
import {Selection} from "d3-selection";
import {area} from "d3-shape";

const MARGIN = {top: 20, right: 5, bottom: 20, left: 5};

/// Brushable time-series graph. Used to show cpu-usage over time
/// and select a time range to drill down into
export class TimeSeriesSelector {
    public x_scale: ScaleLinear<number, number>;
    public y_scale: ScaleLinear<number, number>;
    public brush: BrushBehavior<{}>;

    public selected: number[] = [0, 0];
    public loaded: number[] = [0, 0];
    public total_time_range: number[] = [0, 1];
    public width: number;
    public height: number;

    public group: Selection<SVGGElement, {}, null, undefined>;
    public stats_group: Selection<SVGGElement, {}, null, undefined>;

    public brushing: boolean = false;
    public brush_timeout: NodeJS.Timeout = null;

    public load: (start: number, end: number) => void;

    protected data: any;

    constructor(public element: HTMLElement) {
        var svg = select(this.element).append("svg")
            .attr("class", "timescale")
            .attr("width", this.element.offsetWidth)
            .attr("height", 80);

        this.load = (start: number, end: number) => {  }

        // some elements (scale/handles) will extend past these width here
        // so we're creating the SVG and then translating main elements to create a
        // the margin (rather than putting in div)
        this.width = +svg.attr("width") - MARGIN.left - MARGIN.right;
        this.height = +svg.attr("height") - MARGIN.top - MARGIN.bottom;
        this.group = svg.append("g").attr("transform", "translate(" + MARGIN.left + "," + MARGIN.top + ")");
        this.stats_group = this.group.append("g");

        this.x_scale = scaleLinear()
            .domain([0, 1])
            .range([0, this.width]);

        this.y_scale = scaleLinear()
            .domain ([0, 1])
            .range([this.height, 0]);

        let load_selected = () => {
            this.brushing = false;
            if ((Math.abs(this.selected[0] - this.loaded[0]) > 0.0001) ||
                (Math.abs(this.selected[1] - this.loaded[1]) > 0.0001)) {
                this.loaded = this.selected;
                this.load(this.loaded[0], this.loaded[1]);
            }
        }

        let set_brush_timeout = () => {
            this.brush_timeout = setTimeout(load_selected, 1000);
        };

        this.brush = brushX()
            .extent([[0, 0], [this.width, this.height]])
            .on("start", () => {
                this.brushing = true;
                set_brush_timeout();
            })
            .on("brush", () => {
                clearTimeout(this.brush_timeout);
                set_brush_timeout();
                if (event.selection !== null) {
                    this.selected = event.selection.map(this.x_scale.invert, this.x_scale);
                }
            })
            .on("end", () => {
                this.brushing = false;
                clearTimeout(this.brush_timeout);
                load_selected();
            });

        this.group.append("g")
            .attr("class", "brush")
            .call(this.brush);

        this.group.append("g")
            .attr("class", "axis");

        // make handles somewhat visible
        this.group.selectAll(".handle")
            .attr("stroke", "#888")
            .attr("stroke-opacity", .9)
            .attr("stroke-width", 1)
            .attr("fill", "#AAA")
            .attr("fill-opacity", .7)

        this.group.selectAll(".selection")
            .attr("fill-opacity", .15)
            .attr("stroke-opacity", .2);
    }

    public resize() {
        let width = this.element.offsetWidth;
        select(this.element).select("svg").attr("width", width);

        this.width = width - MARGIN.left - MARGIN.right;
        this.x_scale.range([0, this.width]);
        this.brush.extent([[0, 0], [this.width, this.height]]);

        // hack: transition in update seems to mess up selected somehow, override
        this.group.select(".brush").call(this.brush.move as any, this.selected.map(this.x_scale));

        this.update(this.data);
    }

    public update(data: any) {
        if (this.brushing) {
            return;
        }

        this.data = data;
        let elapsed = data[0].values.length / 10;
        this.total_time_range = [0, elapsed];
        let transition = true;

        if (this.selected[0] == 0 && this.selected[1] == 0) {
            this.selected = [0, elapsed];
            transition = false;
        }
        this.x_scale.domain(this.total_time_range);

        let cpu_scale = scaleLinear().domain([0, data[0].values.length - 1])
                          .range(this.total_time_range);

        var l = area()
            .y0(this.height)
            .y1((d:any) => this.y_scale(d))
            .x((d: any, i: number) =>  this.x_scale(cpu_scale(i)));

        let stats = this.stats_group.selectAll(".stat")
            .data(data);

        let enter = stats.enter()
            .append("g")
            .attr("class", "stat");

        enter
            .append("path")
            .attr("stroke", (d: any) => d.colour)
            .attr("fill", (d: any) => d.colour)
            .attr("fill-opacity", .05)
            .attr("stroke-width", 1)
            .attr("d", (d: any) => {
                return l(d.values)
            });

        enter.append("text")
            .attr("x", (d:any) => d.legend_x + 8)
            .attr("y", -8)
            .style("font-size", "10px")
            .text((d: any) => d.name);

        enter.append("rect")
            .attr("x", (d: any) => d.legend_x)
            .attr("y", -14)
            .attr("height", 5)
            .attr("width", 5)
            .attr("stroke", (d: any) => d.colour)
            .attr("fill", (d: any) => d.colour)
            .attr("fill-opacity", .1)
            .attr("stroke-width", 1);

        if (transition) {
            stats.select("path").transition()
                .attr("d", (d: any) => {
                    return l(d.values)
                });

            stats.select("text").transition()
                .text((d: any) => d.name);

            this.group.select(".brush")
                .call(this.brush)
                .transition()
                .call(this.brush.move as any, this.selected.map(this.x_scale));

            this.group.select(".axis")
                .attr("transform", "translate(0," + this.height + ")")
                .transition()
                .call(axisBottom(this.x_scale).tickFormat(d => d + "s") as any);
        } else {
            stats.select("path")
                .attr("d", (d: any) => {
                    return l(d.values)
                });

            stats.select("text")
                .text((d: any) => d.name);

            this.group.select(".brush")
                .call(this.brush.move as any, this.selected.map(this.x_scale));

            this.group.select(".axis")
                .attr("transform", "translate(0," + this.height + ")")
                .call(axisBottom(this.x_scale).tickFormat(d => d + "s") as any);
            }
    }

    protected set_brushtimeout() {
    }
}