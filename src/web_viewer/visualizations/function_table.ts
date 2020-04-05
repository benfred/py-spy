import {select} from "d3-selection";

export class FunctionTable {
    public ascending = false;
    public sort_column = 2;
    public sampling_rate = 100;

    public update(table_element: HTMLElement, table: any, total_samples: number): void {
        sort_table(table, this.sort_column, this.ascending);
        this.update_table(table_element, table, total_samples);
    }

    protected update_table(table_element: HTMLElement, data: any, total_samples: number): void {
        let table_selection = select(table_element);
        let line_numbers = (document.getElementById("include_lines") as HTMLInputElement).checked;

        // TODO: probably could exit/remove after real
        // data join below, but seems to fail
        let body = table_selection.select("tbody");
        body.selectAll("tr").data([]).exit().remove();

        let titles = ["name", line_numbers ? "filename:line" : "filename", "own", "total"];
        let table = this;
        function click_handler(d: any, i: number) {
            table_selection.select('thead').select("tr").selectAll('th').attr("class", "");
            if (i == table.sort_column) {
                table.ascending = !table.ascending;
            }
            this.className = table.ascending ? "asc" : "desc"
            table.sort_column = i;
            sort_table(data, table.sort_column, table.ascending);
            table.update_table(table_element, data, total_samples);
        }

        table_selection.select('thead').select("tr")
            .selectAll('th')
            .html((d: any, i:number) => "<span>" + titles[i] + "</span>")
            .on('click', click_handler)
            .data(titles).enter()
            .append("th")
            .html((d: any) => "<span>" + d + "</span>")
            .attr("class", (d:any, i:number) => i == this.sort_column ? (this.ascending ? "asc": "desc") : "")
            .on('click', click_handler);

        let rows = body.selectAll("tr")
            .data(data)
            .enter()
            .append("tr");

        rows.append("td").append("a")
            .attr("href", (d:any) => "/function/" + escape(d.frame.name) + "?f=" + escape(d.frame.short_filename))
            .text((d:any) => d.frame.name);

        rows.append("td")
            // .append("a").attr("href", (d:any) => "/file?f=" + escape(d.short_filename))
            .text((d:any) => d.frame.short_filename + (line_numbers ? ":" + d.frame.line : ""))

        rows.append("td").text((d:any) => (100 * d.own_count / total_samples).toPrecision(3) + "%");
        rows.append("td").text((d:any) => (100 * d.total_count / total_samples).toPrecision(3) + "%");
        // rows.append("td").text((d:any) => (d.own_count / this.sampling_rate).toPrecision(3) + "s");
        // rows.append("td").text((d:any) => (d.total_count / this.sampling_rate).toPrecision(3) + "s");
    }
}

function sort_table(data: any, col_index: number, ascending: boolean) {
    if (col_index == 0) {
        data.sort((a: any, b: any) => a.frame.name.localeCompare(b.frame.name));
    } else if (col_index == 1) {
        data.sort((a: any, b: any) => a.frame.short_filename.localeCompare(b.frame.short_filename) || (a.frame.line - b.frame.line));
    } else if (col_index == 2) {
        data.sort((a: any, b: any) => (a.own_count - b.own_count) || (a.total_count - b.total_count));
    } else if (col_index == 3) {
        data.sort((a: any, b: any) => (a.total_count - b.total_count) || (a.own_count - b.own_count));
    }

    if (!ascending) {
        data.reverse();
    }
}
