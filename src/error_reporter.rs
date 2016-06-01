use std::fmt;
use std::rc::Rc;

use text_buffer_2d::*;
use term;

use codemap::{self, Span, CharPos};

#[derive(Clone, Debug)]
struct SpanLabel {
    /// The span we are going to include in the final snippet.
    pub span: Span,

    /// Is this a primary span? This is the "locus" of the message,
    /// and is indicated with a `^^^^` underline, versus `----`.
    pub is_primary: bool,

    /// What label should we attach to this span (if any)?
    pub label: Option<String>,
}

pub struct ErrorReporter {
    level: Level,
    primary_span: Span,
    primary_msg: String,
    span_labels: Vec<SpanLabel>,
    cm: Rc<codemap::CodeMap>,
}

#[derive(Clone, Debug)]
struct Line {
    // Use a span here as a way to acquire this line later
    span: Span,
    annotations: Vec<Annotation>,
}

#[derive(Clone, Debug, PartialOrd, Ord, PartialEq, Eq)]
struct Annotation {
    /// Start column, 0-based indexing -- counting *characters*, not
    /// utf-8 bytes. Note that it is important that this field goes
    /// first, so that when we sort, we sort orderings by start
    /// column.
    start_col: usize,

    /// End column within the line (exclusive)
    end_col: usize,

    /// Is this annotation derived from primary span
    is_primary: bool,

    /// Is this a large span minimized down to a smaller span
    is_minimized: bool,

    /// Optional label to display adjacent to the annotation.
    label: Option<String>,
}

fn check_old_school() -> bool {
    false
}

impl ErrorReporter {
    pub fn span_label(&mut self, span: Span, label: Option<String>) -> &mut ErrorReporter {
        self.span_labels.push(SpanLabel {
            span: span,
            is_primary: (span == self.primary_span),
            label: label,
        });
        self
    }

    pub fn new(level: Level,
               msg: String,
               primary_span: Span,
               cm: Rc<codemap::CodeMap>)
               -> ErrorReporter {

        ErrorReporter {
            level: level,
            primary_span: primary_span,
            primary_msg: msg,
            span_labels: vec![],
            cm: cm,
        }
    }

    fn render_header(&mut self, buffer: &mut TextBuffer2D) {
        // Header line 1: error: the error message [ENUM]
        buffer.append(0, &self.level.to_string(), Style::Level(self.level));
        buffer.append(0, ": ", Style::HeaderMsg);
        buffer.append(0, &self.primary_msg.clone(), Style::HeaderMsg);

        // Header line 2: filename:line:col (we'll write the --> later)
        buffer.append(1,
                      &self.cm.span_to_string(self.primary_span),
                      Style::LineAndColumn);
    }

    fn render_source_lines(&mut self, buffer: &mut TextBuffer2D) {
        use std::collections::HashMap;

        let mut file_map: HashMap<String, HashMap<usize, Line>> = HashMap::new();

        // Convert our labels+spans into the annotations we'll be displaying to the user.
        // To do this, we'll build up a HashMap for each file we need to display
        // in the hashmap, we'll build up our annotated source lines
        for span_label in &self.span_labels {
            let filename = self.cm.span_to_filename(span_label.span);
            let mut line_map = file_map.entry(filename).or_insert(HashMap::new());

            let lo = self.cm.lookup_char_pos(span_label.span.lo);
            let hi = self.cm.lookup_char_pos(span_label.span.hi);
            // If the span is multi-line, simplify down to the span of one character
            let (start_col, mut end_col, is_minimized) = if lo.line != hi.line {
                (lo.col, CharPos(lo.col.0 + 1), true)
            } else {
                (lo.col, hi.col, false)
            };

            // Watch out for "empty spans". If we get a span like 6..6, we
            // want to just display a `^` at 6, so convert that to
            // 6..7. This is degenerate input, but it's best to degrade
            // gracefully -- and the parser likes to supply a span like
            // that for EOF, in particular.
            if start_col == end_col {
                end_col.0 += 1;
            }

            let line_entry = (*line_map).entry(lo.line).or_insert(Line {
                span: span_label.span.clone(),
                annotations: vec![],
            });

            (*line_entry).annotations.push(Annotation {
                start_col: lo.col.0,
                end_col: hi.col.0,
                is_primary: span_label.is_primary,
                is_minimized: is_minimized,
                label: span_label.label.clone(),
            })
        }

        // Now that we have lines with their annotations, we can sort the lines we know about,
        // walk through them, and begin rendering the source block in the error
        // TODO: we should print the primary file first
        for fname in file_map.keys() {
            let mut all_lines: Vec<&usize> = file_map[fname].keys().collect();
            all_lines.sort();

            // TODO: while we're at it, go ahead and figure out the largest line number
            // so we can easily align the line number column

            for line in all_lines {
                self.render_source_line(buffer, &file_map[fname][line]);
            }
        }
        // println!("{:?}", file_map);
    }

    fn render_source_line(&mut self, buffer: &mut TextBuffer2D, line: &Line) {
        let result = self.cm.span_to_lines(line.span).unwrap();
        let source_string = result.file
            .get_line(result.lines.first().unwrap().line_index)
            .unwrap_or("");

        let line_offset = buffer.num_lines();

        // First create the source line we will highlight.
        buffer.append(line_offset, &source_string, Style::Quotation);

        if line.annotations.is_empty() {
            return;
        }

        // We want to display like this:
        //
        //      vec.push(vec.pop().unwrap());
        //      ---      ^^^               _ previous borrow ends here
        //      |        |
        //      |        error occurs here
        //      previous borrow of `vec` occurs here
        //
        // But there are some weird edge cases to be aware of:
        //
        //      vec.push(vec.pop().unwrap());
        //      --------                    - previous borrow ends here
        //      ||
        //      |this makes no sense
        //      previous borrow of `vec` occurs here
        //
        // For this reason, we group the lines into "highlight lines"
        // and "annotations lines", where the highlight lines have the `~`.

        // let mut highlight_line = Self::whitespace(&source_string);
        let old_school = check_old_school();

        // Sort the annotations by (start, end col)
        let mut annotations = line.annotations.clone();
        annotations.sort();

        // Next, create the highlight line.
        for annotation in &annotations {
            if old_school {
                for p in annotation.start_col..annotation.end_col {
                    if p == annotation.start_col {
                        buffer.putc(line_offset + 1,
                                    p,
                                    '^',
                                    if annotation.is_primary {
                                        Style::UnderlinePrimary
                                    } else {
                                        Style::OldSkoolNote
                                    });
                    } else {
                        buffer.putc(line_offset + 1,
                                    p,
                                    '~',
                                    if annotation.is_primary {
                                        Style::UnderlinePrimary
                                    } else {
                                        Style::OldSkoolNote
                                    });
                    }
                }
            } else {
                for p in annotation.start_col..annotation.end_col {
                    if annotation.is_primary {
                        buffer.putc(line_offset + 1, p, '^', Style::UnderlinePrimary);
                        if !annotation.is_minimized {
                            buffer.set_style(line_offset, p, Style::UnderlinePrimary);
                        }
                    } else {
                        buffer.putc(line_offset + 1, p, '-', Style::UnderlineSecondary);
                        if !annotation.is_minimized {
                            buffer.set_style(line_offset, p, Style::UnderlineSecondary);
                        }
                    }
                }
            }
        }

        // Now we are going to write labels in. To start, we'll exclude
        // the annotations with no labels.
        let (labeled_annotations, unlabeled_annotations): (Vec<_>, _) = annotations.into_iter()
            .partition(|a| a.label.is_some());

        // If there are no annotations that need text, we're done.
        if labeled_annotations.is_empty() {
            return;
        }
        if old_school {
            return;
        }

        // Now add the text labels. We try, when possible, to stick the rightmost
        // annotation at the end of the highlight line:
        //
        //      vec.push(vec.pop().unwrap());
        //      ---      ---               - previous borrow ends here
        //
        // But sometimes that's not possible because one of the other
        // annotations overlaps it. For example, from the test
        // `span_overlap_label`, we have the following annotations
        // (written on distinct lines for clarity):
        //
        //      fn foo(x: u32) {
        //      --------------
        //             -
        //
        // In this case, we can't stick the rightmost-most label on
        // the highlight line, or we would get:
        //
        //      fn foo(x: u32) {
        //      -------- x_span
        //      |
        //      fn_span
        //
        // which is totally weird. Instead we want:
        //
        //      fn foo(x: u32) {
        //      --------------
        //      |      |
        //      |      x_span
        //      fn_span
        //
        // which is...less weird, at least. In fact, in general, if
        // the rightmost span overlaps with any other span, we should
        // use the "hang below" version, so we can at least make it
        // clear where the span *starts*.
        let mut labeled_annotations = &labeled_annotations[..];
        match labeled_annotations.split_last().unwrap() {
            (last, previous) => {
                if previous.iter()
                    .chain(&unlabeled_annotations)
                    .all(|a| !overlaps(a, last)) {
                    // append the label afterwards; we keep it in a separate
                    // string
                    let highlight_label: String = format!(" {}", last.label.as_ref().unwrap());
                    if last.is_primary {
                        buffer.append(line_offset + 1, &highlight_label, Style::LabelPrimary);
                    } else {
                        buffer.append(line_offset + 1, &highlight_label, Style::LabelSecondary);
                    }
                    labeled_annotations = previous;
                }
            }
        }

        // If that's the last annotation, we're done
        if labeled_annotations.is_empty() {
            return;
        }

        for (index, annotation) in labeled_annotations.iter().enumerate() {
            // Leave:
            // - 1 extra line
            // - One line for each thing that comes after
            let comes_after = labeled_annotations.len() - index - 1;
            let blank_lines = 3 + comes_after;

            // For each blank line, draw a `|` at our column. The
            // text ought to be long enough for this.
            for index in 2..blank_lines {
                if annotation.is_primary {
                    buffer.putc(line_offset + index,
                                annotation.start_col,
                                '|',
                                Style::UnderlinePrimary);
                } else {
                    buffer.putc(line_offset + index,
                                annotation.start_col,
                                '|',
                                Style::UnderlineSecondary);
                }
            }

            if annotation.is_primary {
                buffer.puts(line_offset + blank_lines,
                            annotation.start_col,
                            annotation.label.as_ref().unwrap(),
                            Style::LabelPrimary);
            } else {
                buffer.puts(line_offset + blank_lines,
                            annotation.start_col,
                            annotation.label.as_ref().unwrap(),
                            Style::LabelSecondary);
            }
        }
    }

    pub fn render(&mut self) -> Vec<Vec<StyledString>> {
        let mut buffer = TextBuffer2D::new();

        self.render_header(&mut buffer);
        self.render_source_lines(&mut buffer);

        // let mut current_line = 2;
        // println!("{:?}", self.cm.lookup_char_pos(self.primary_span.lo));
        // let result = self.cm.span_to_lines(self.primary_span).unwrap();
        // for line in result.lines {
        // println!("{:?}", result.file.get_line(line.line_index));
        // }
        //

        buffer.render()
    }
}

fn overlaps(a1: &Annotation, a2: &Annotation) -> bool {
    (a2.start_col..a2.end_col).contains(a1.start_col) ||
    (a1.start_col..a1.end_col).contains(a2.start_col)
}
