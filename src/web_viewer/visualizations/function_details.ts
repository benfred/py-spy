import {json} from "d3-fetch";
import {select} from "d3-selection";
import {display_error_message} from "./utils";

declare var hljs: any;

export class FunctionDetails {
    public data: any;
    public hierarchy: any;
    public lines: any;

    constructor(public timescale_element: HTMLElement,
                public name: string, public short_filename: string, public filename: string) {
        let url = "/api/function_info?file=" + escape(short_filename) + "&function=" + escape(name) + "&include_lines=1&include_frames=idle";
        json(url)
            .then((d: any) => {
                // Doesn't seem like we can get the response body in the catch handler on this promise
                // so we're putting the error message in the json field instead
                if ('error' in d) {
                    display_error_message(d.error);
                    return;
                }
                this.data = d;

                let lines = d.contents.split("\n");
                this.lines = lines;

                let line_to_node: Record<number, any> = {};
                let total_samples = 0;
                let own_samples = 0;
                for (let node of d.flattened) {
                    line_to_node[node.frame.line] = node;
                    total_samples += node.total_count;
                    own_samples += node.own_count;
                }

                for (let code_block of get_code_blocks(lines, this.data.flattened, this.name)) {
                    let first_line = code_block[0]+1;
                    let next_line = code_block[1]+ 1;
                    let parent = code_block[2]+1;

                    let block = select(".functionheatmap");

                    let table = block.append("pre")
                        .classed("hljs", true)
                        .append("table");

                    if (parent > 0) {
                        let header = table.append("thead");
                        header.append("th");
                        header.append("th")
                            .text(parent)
                            .classed("linenumber", true);
                        header.append("th")
                            .classed("parentcode", true)
                            .text(lines[parent-1] + "\n...");
                    }

                    let block_lines = highlight_lines(lines.slice(first_line - 1, next_line));

                    let rows = table.append("tbody")
                        .selectAll("tr")
                        .data(block_lines)
                        .enter()
                        .append("tr");

                    rows.append("td")
                        .classed("percent", true)
                        .text((d,i) => {
                            let node = line_to_node[i + first_line];
                            if (node == undefined) {
                                return ""
                            }
                            return (100.0 * node.total_count / total_samples).toLocaleString() + "%";
                        });

                    rows.append("td")
                        .classed("linenumber", true)
                        .text((d,i) => i + first_line);

                    rows.append("td")
                        .append("code")
                        .html((d: any) => d);
                }
            })
            .catch(err => {
                display_error_message(err);
                console.log("Failed to get", url, err);
            });
    }
}

function get_code_blocks(lines: string[], flattened: any[], name: string): any[] {
    let indent_levels = get_indent_level(lines);
    let hierarchy = Array(lines.length).fill(-1);
    function get_parent(line: number): number {
        // if we've already calculated the parent for this line, use that
        if (hierarchy[line] != -1) {
            return hierarchy[line];
        }

        // scan backwards for first non-whitespace/non-comment line that has a lower indent level.
        let indent = indent_levels[line];
        for (let previous = line - 1; previous >= 0; previous--) {
            // Skip whitespace
            let previous_indent = indent_levels[previous];
            if (previous_indent == -1) {
                continue;
            }

            // just in case the file has changed - start at the first non-whitespace comment line
            if (indent == -1) {
                indent = indent_levels[previous];
                continue;
            }

            if (previous_indent < indent) {
                hierarchy[line] = previous;
                return previous;
            }
        }
        return -1;
    }

    // Get a list of 'root' lines that are parents of profiled lines
    let roots: Set<number> = new Set();
    for (let node of flattened) {
        let parent = node.frame.line - 1;
        while (parent > 0) {
            if (lines[parent].includes(" " + name + "(")) {
                break;
            }
            let next_parent = get_parent(parent);
            if (next_parent == -1) break;
            parent = next_parent;
        }
        roots.add(parent);
    }

    // Get blocks of codes from the root lines
    let code_blocks: number[][] = []
    for (let first_line of roots) {
        // Figure out the range of the block
        let next_line = first_line;
        let root_indent = indent_levels[first_line];
        while (next_line < indent_levels.length) {
            let indent = indent_levels[next_line + 1];
            if (indent != -1 && indent <= root_indent) {
                break;
            }
            next_line += 1;
        }
        // Remove trailing comments/whitespace lines
        while (next_line > first_line) {
            let indent = indent_levels[next_line];
            if (indent != -1) {
                break;
            }
            next_line -= 1;
        }
        code_blocks.push([first_line, next_line, get_parent(first_line)]);
    }
    code_blocks.sort((a,b) => a[0] - b[0]);

    // if multiple code blocks are close to one another, merge together
    let previous_line = code_blocks[0][1];
    let previous_target = 0;
    for (let i = 1; i < code_blocks.length; ++i) {
        let first_line = code_blocks[i][0];
        if (first_line < previous_line + 8) {
            code_blocks[previous_target][1] = code_blocks[i][1];
            code_blocks[i][0] = -1;
        } else {
            previous_target = i;
        }
        previous_line = code_blocks[i][1];
    }
    return code_blocks.filter((x) => x[0] >= 0);
}

function get_indent_level(lines: string[]): number[] {
    let indent_level = Array(lines.length).fill(-1);
    for (let i = 0; i < lines.length; ++i) {
        let line = lines[i];
        let indent = line.search(/\S|$/);
        if ((indent == line.length) || (line[indent] == "#")) {
            continue;
        }
        indent_level[i] = indent;
    }
    return indent_level;
}

function highlight_lines(lines: string[]): string[] {
    let prettified = [];
    let in_multiline = false;
    for (let line of lines) {
        // hack for multilines strings
        let mismatched = (line.match(/"""/g) || []).length % 2 == 1;

        if (in_multiline) {
            prettified.push("<span class='hljs-string'>" + line.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;') + "</span>");
        } else {
            prettified.push(hljs.highlight("python", line).value);
        }
        if (mismatched) {
            in_multiline = !in_multiline;
        }
    }
    return prettified;
}
