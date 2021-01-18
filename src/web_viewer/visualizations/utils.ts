import {select, selectAll} from "d3-selection";

export function display_error_message(message: string): void {
    let error = select(".error_info")
        .style("display", "block");

    error.select(".error_text")
        .text(message)

    error.select(".errordismiss")
        .on("click", () => error.style("display", "none"))
}
