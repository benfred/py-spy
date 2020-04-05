import {mouse, select} from "d3-selection";
import {scaleLinear} from "d3-scale";
import {partition, hierarchy} from "d3-hierarchy";
import {interpolateYlOrRd} from "d3-scale-chromatic";


export class Flamegraph {
    public sampling_rate = 100;
    public cell_height = 21; // should be 1px more than div height in css
    public min_text_width = 64;

    public update(element: HTMLElement, data: any, transition: boolean){
        calculate_ids(data, null);
        this.update_flamegraph(element, data, transition, null);
    }

    protected update_flamegraph(element: HTMLElement, data: any, transition: boolean, zoom: any): void {
        const width = element.offsetWidth;
        let partition_layout = partition()
            .padding(1)
            .size([width, 1]);

        let root = hierarchy(data);
        root.sort((a, b) => compare_frame(a.data.frame, b.data.frame));
        partition_layout(root);

        let height = Math.max(screen.height - element.offsetTop, (root.height + 1) * this.cell_height) + "px";
        select(element).style("height", height);

        let x_scale = scaleLinear()
            .range([0, width])
            .domain([0, width]);

        let zoom_depth = -1;
        if (zoom) {
            zoom_depth = zoom.depth;
            x_scale.domain([zoom.x0, zoom.x1]);
        }

        // Draw the flame graph with 1 div per node
        let divs = select(element)
            .selectAll('div')
            .data(root.descendants(), (d:any) => d.data.id);

        let new_divs = divs
            .enter()
            .append("div")
            .style("background-color", (d:any) => get_colour(d.data.frame.name))
            .style("left", (d:any) => x_scale(d.x0) + "px")
            .style('top', (d:any) => (d.depth * this.cell_height) + "px")
            .on("click", (node: any) => this.update_flamegraph(element, data, true, node))
            .on("mousemove", (node: any) => {
                const cursor = mouse(element).map((d: number) => +d);
                let tooltip = select(".tooltip");
                tooltip.classed("hidden", false)
                    .attr("style", `left:${cursor[0] + element.offsetLeft + 10}px;top:${cursor[1] + element.offsetTop+10}px`);

                let root = node;
                while (root.parent) { root = root.parent; }
                const percent = 100.0 * node.data.value / root.value;
                const seconds = node.data.value / this.sampling_rate;
                tooltip.select(".tooltippercent").text(percent.toPrecision(3) + "%");
                // tooltip.select(".tooltiptime").text(seconds.toPrecision(3) + "s");

                const frame = node.data.frame;
                let filename = "";
                if (frame.short_filename) {
                    filename = frame.short_filename  ? `${node.data.frame.short_filename}` : "";
                    let line_numbers = (document.getElementById("include_lines") as HTMLInputElement).checked;
                    filename += line_numbers && frame.line ? `:${node.data.frame.line}` : "";
                }
                tooltip.select(".tooltipcontent").text(filename);
                tooltip.select(".tooltiptitle").text(node.data.frame.name);
            })
            .on("mouseout", () => {
                select(".tooltip").classed("hidden", true);
            });

        new_divs
            .append("a")
            .style("display", (d:any) => (x_scale(d.x1) - x_scale(d.x0)) > this.min_text_width ? null : "none")
            .text((d) => d.data.name)
            .filter((d) => d.data.frame.filename.length)
            .attr("href", (d) => "/function/" + escape(d.data.frame.name) + "?f=" + escape(d.data.frame.short_filename))

        if (transition) {
            new_divs.style("width", "0px").style("top", height)
                .transition()
                .duration(1000)
                .style('top', (d:any) => (d.depth * this.cell_height) + "px")
                .style('width', (d:any) => (x_scale(d.x1) - x_scale(d.x0)) + "px");
        } else {
            new_divs.style('width', (d:any) => (x_scale(d.x1) - x_scale(d.x0)) + "px");
        }

        // override click / display handlers here (data+zoom could have changed since node was created)
        divs.on("click", (node: any) => this.update_flamegraph(element, data, true, node))
            .select("a").style("display", (d:any) => (x_scale(d.x1) - x_scale(d.x0)) > this.min_text_width ? null : "none");

        divs.transition()
            .duration(transition ? 1000 : 0)
            .style("left", (d:any) => {
                let x0 = x_scale(d.x0);
                if ((x0 < 0) && (x_scale(d.x1) > 0)) {
                    return "0px";
                }
                return x0 + "px";
            })
            .style("opacity", (d:any) => d.depth >= zoom_depth ? 1.0 : 0.4)
            .style("top", (d:any) => (d.depth * this.cell_height) + "px")
            .style("width", (d:any) => (x_scale(d.x1) - x_scale(d.x0)) + "px")

        divs.exit()
            .transition()
            .duration(transition ? 1000 : 0)
            .style("width", "0px")
            .style("top", height)
            .remove();
    }
}


// annotate each node with a id (used for data joins in transitions)
function calculate_ids(node: any, parent_id: String): void {
    node.id = parent_id ? parent_id + "|" + node.name : node.name;

    if (!node.children) {
        return;
    }
    for (let child of node.children) {
        calculate_ids(child, node.id);
    }
}

function compare_frame(a: any, b: any): number {
    const filecmp = (a.filename && b.filename) ? a.filename.localeCompare(b.filename) : 0;
    return filecmp || a.name.localeCompare(b.name) || (a.line - b.line)
}

function get_colour(name: string) {
    return interpolateYlOrRd(0.8 * generate_hash(name));
}

// generate_hash copied from https://github.com/spiermar/d3-flame-graph
// Released under the apache license 2.0
function generate_hash(name: string) {
    // Return a vector (0.0->1.0) that is a hash of the input string.
    // The hash is computed to favor early characters over later ones, so
    // that strings with similar starts have similar vectors. Only the first
    // 6 characters are considered.
    const MAX_CHAR = 6

    var hash = 0
    var maxHash = 0
    var weight = 1
    var mod = 10

    if (name) {
        for (var i = 0; i < name.length; i++) {
            if (i > MAX_CHAR) { break }
            hash += weight * (name.charCodeAt(i) % mod)
            maxHash += weight * (mod - 1)
            weight *= 0.70
        }
        if (maxHash > 0) { hash = hash / maxHash }
    }
    return hash
}