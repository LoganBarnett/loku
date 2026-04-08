module Page.Player exposing (Model, Msg(..), init, update, view, mediaErrorMessage)

import Api exposing (Entry(..))
import Route
import Html exposing (..)
import Html.Attributes exposing (..)
import Html.Events exposing (on)
import Http
import Json.Decode as D


type Model
    = Loading Bool String
    | Loaded PlayerState
    | Failed String


type alias PlayerState =
    { path : String
    , title : String
    , uploadDate : Maybe String
    , durationSecs : Maybe Float
    , compatPath : Maybe String
    , description : Maybe String
    , channel : Maybe String
    , channelUrl : Maybe String
    , webpageUrl : Maybe String
    , viewCount : Maybe Int
    , bufferFraction : Float
    , mediaError : Maybe MediaErrorCode
    , canWebm : Bool
    }


{-| MediaError.code values from the HTML media element spec.
-}
type MediaErrorCode
    = ErrAborted
    | ErrNetwork
    | ErrDecode
    | ErrSrcNotSupported
    | ErrUnknown Int


type Msg
    = GotListing (Result Http.Error Api.DirListing)
    | VideoCanPlay
    | VideoProgress Float
    | MediaError MediaErrorCode


init : Bool -> String -> ( Model, Cmd Msg )
init canWebm path =
    let
        -- Fetch the parent directory listing to retrieve video metadata.
        parentPath =
            path
                |> String.split "/"
                |> List.reverse
                |> List.drop 1
                |> List.reverse
                |> String.join "/"
    in
    ( Loading canWebm path, Api.getBrowse parentPath GotListing )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotListing (Ok listing) ->
            let
                path =
                    loadingPath model

                canWebm =
                    loadingCanWebm model

                matched =
                    listing.entries
                        |> List.filterMap
                            (\entry ->
                                case entry of
                                    Video v ->
                                        if v.path == path then
                                            Just
                                                { path = v.path
                                                , title =
                                                    Maybe.withDefault v.name
                                                        v.title
                                                , uploadDate = v.uploadDate
                                                , durationSecs = v.durationSecs
                                                , compatPath = v.compatPath
                                                , description = v.description
                                                , channel = v.channel
                                                , channelUrl = v.channelUrl
                                                , webpageUrl = v.webpageUrl
                                                , viewCount = v.viewCount
                                                , bufferFraction = 0
                                                , mediaError = Nothing
                                                , canWebm = canWebm
                                                }

                                        else
                                            Nothing

                                    _ ->
                                        Nothing
                            )
                        |> List.head
                        |> Maybe.withDefault
                            { path = path
                            , title = path
                            , uploadDate = Nothing
                            , durationSecs = Nothing
                            , compatPath = Nothing
                            , description = Nothing
                            , channel = Nothing
                            , channelUrl = Nothing
                            , webpageUrl = Nothing
                            , viewCount = Nothing
                            , bufferFraction = 0
                            , mediaError = Nothing
                            , canWebm = canWebm
                            }
            in
            ( Loaded matched, Cmd.none )

        GotListing (Err _) ->
            -- Metadata load failed; still show the player with minimal info.
            let
                path =
                    loadingPath model

                canWebm =
                    loadingCanWebm model
            in
            ( Loaded
                { path = path
                , title = path
                , uploadDate = Nothing
                , durationSecs = Nothing
                , compatPath = Nothing
                , description = Nothing
                , channel = Nothing
                , channelUrl = Nothing
                , webpageUrl = Nothing
                , viewCount = Nothing
                , bufferFraction = 0
                , mediaError = Nothing
                , canWebm = canWebm
                }
            , Cmd.none
            )

        VideoCanPlay ->
            case model of
                Loaded state ->
                    ( Loaded { state | bufferFraction = 1 }, Cmd.none )

                _ ->
                    ( model, Cmd.none )

        VideoProgress fraction ->
            case model of
                Loaded state ->
                    ( Loaded { state | bufferFraction = fraction }, Cmd.none )

                _ ->
                    ( model, Cmd.none )

        MediaError code ->
            case model of
                Loaded state ->
                    ( Loaded { state | mediaError = Just code }, Cmd.none )

                _ ->
                    ( model, Cmd.none )


view : Model -> Html Msg
view model =
    case model of
        Loading _ _ ->
            p [ style "padding" "1rem" ] [ text "Loading…" ]

        Failed err ->
            p [ style "padding" "1rem", style "color" "var(--color-error)" ]
                [ text ("Error: " ++ err) ]

        Loaded state ->
            let
                parentPath =
                    state.path
                        |> String.split "/"
                        |> List.reverse
                        |> List.drop 1
                        |> List.reverse
                        |> String.join "/"
            in
            div [ style "padding" "1rem" ]
                [ a
                    [ href (Route.toString (Route.Browse { path = parentPath, query = "", page = 1 }))
                    , style "margin-bottom" "1rem"
                    , style "display" "inline-block"
                    ]
                    [ text "← Back" ]
                , case state.mediaError of
                    Just code ->
                        div
                            [ style "background" "var(--color-surface)"
                            , style "padding" "2rem"
                            , style "max-width" "960px"
                            , style "text-align" "center"
                            ]
                            [ p [ style "color" "var(--color-error)" ]
                                [ text (mediaErrorMessage code) ]
                            , a
                                [ href (Api.videoUrl state.path)
                                , attribute "download" ""
                                ]
                                [ text "Download to play in VLC or another media player" ]
                            ]

                    Nothing ->
                        let
                            videoAttrs =
                                [ controls True
                                , on "canplay" (D.succeed VideoCanPlay)
                                , style "width" "100%"
                                , style "max-width" "960px"
                                , style "display" "block"
                                ]

                            ( extraAttrs, sources ) =
                                case state.compatPath of
                                    Nothing ->
                                        -- Single source: attach error decoder directly
                                        -- to the video element where target.error.code
                                        -- is available.
                                        ( [ src (Api.videoUrl state.path)
                                          , on "error" (D.map MediaError mediaErrorDecoder)
                                          ]
                                        , []
                                        )

                                    Just cp ->
                                        -- Two sources: browser picks the first it can
                                        -- play. The type_ attribute lets the browser
                                        -- skip formats it cannot decode without
                                        -- downloading the file to probe it.
                                        -- Error fires on the last source only if both
                                        -- fail; source elements have no .error.code so
                                        -- we use a fixed code.
                                        let
                                            primaryMime =
                                                mimeTypeFromPath state.path

                                            ( first, second ) =
                                                if state.canWebm then
                                                    ( ( Api.videoUrl state.path, primaryMime )
                                                    , ( Api.videoUrl cp, "video/mp4" )
                                                    )

                                                else
                                                    ( ( Api.videoUrl cp, "video/mp4" )
                                                    , ( Api.videoUrl state.path, primaryMime )
                                                    )
                                        in
                                        ( []
                                        , [ source [ src (Tuple.first first), type_ (Tuple.second first) ] []
                                          , source
                                                [ src (Tuple.first second)
                                                , type_ (Tuple.second second)
                                                , on "error" (D.succeed (MediaError ErrSrcNotSupported))
                                                ]
                                                []
                                          ]
                                        )
                        in
                        div []
                            [ if state.bufferFraction < 1 then
                                div
                                    [ style "width" "100%"
                                    , style "max-width" "960px"
                                    , style "height" "4px"
                                    , style "background" "var(--color-surface)"
                                    , style "margin-bottom" "0.25rem"
                                    ]
                                    [ div
                                        [ style "height" "100%"
                                        , style "width" (String.fromFloat (state.bufferFraction * 100) ++ "%")
                                        , style "background" "var(--color-link)"
                                        , style "transition" "width 0.3s ease"
                                        ]
                                        []
                                    ]

                              else
                                text ""
                            , video (videoAttrs ++ extraAttrs) sources
                            ]
                , h2 [ style "margin-top" "0.75rem" ] [ text state.title ]
                , case state.uploadDate of
                    Just date ->
                        p [] [ text ("Uploaded: " ++ formatDate date) ]

                    Nothing ->
                        text ""
                , case state.durationSecs of
                    Just secs ->
                        p [] [ text ("Duration: " ++ formatDuration secs) ]

                    Nothing ->
                        text ""
                , case state.viewCount of
                    Just n ->
                        p [] [ text ("Views: " ++ formatViewCount n) ]

                    Nothing ->
                        text ""
                , case state.channel of
                    Just ch ->
                        p []
                            [ text "Channel: "
                            , case state.channelUrl of
                                Just url ->
                                    a [ href url, target "_blank", attribute "rel" "noopener noreferrer" ]
                                        [ text ch ]

                                Nothing ->
                                    text ch
                            ]

                    Nothing ->
                        text ""
                , case state.webpageUrl of
                    Just url ->
                        p []
                            [ a [ href url, target "_blank", attribute "rel" "noopener noreferrer" ]
                                [ text "Watch on YouTube" ]
                            ]

                    Nothing ->
                        text ""
                , case state.description of
                    Just desc ->
                        div
                            [ style "margin-top" "1rem"
                            , style "max-width" "960px"
                            , style "max-height" "14rem"
                            , style "overflow-y" "auto"
                            , style "font-size" "0.9rem"
                            , style "line-height" "1.6"
                            , style "white-space" "pre-wrap"
                            , style "word-break" "break-word"
                            ]
                            (renderDescription desc)

                    Nothing ->
                        text ""
                ]


{-| Strip trailing punctuation characters that are unlikely to be part of a
URL even though they are technically valid. If they were intentional they
would normally be percent-encoded.
-}
stripTrailingPunct : String -> ( String, String )
stripTrailingPunct s =
    let
        punctChars =
            [ '.', ',', ';', ')', ']', '!', '"', '\'', '>', '?' ]

        dropRight str =
            case String.uncons (String.reverse str) of
                Just ( c, rest ) ->
                    if List.member c punctChars then
                        dropRight (String.reverse rest)

                    else
                        str

                Nothing ->
                    str

        url =
            dropRight s
    in
    ( url, String.dropLeft (String.length url) s )


{-| Render a description string, turning http/https tokens into clickable
links. Newlines are preserved by the parent's white-space: pre-wrap style.
-}
renderDescription : String -> List (Html msg)
renderDescription desc =
    let
        isUrl w =
            String.startsWith "http://" w || String.startsWith "https://" w

        renderWord w =
            if isUrl w then
                let
                    ( url, trailing ) =
                        stripTrailingPunct w
                in
                span []
                    [ a
                        [ href url
                        , target "_blank"
                        , attribute "rel" "noopener noreferrer"
                        ]
                        [ text url ]
                    , text trailing
                    ]

            else
                text w

        renderLine line =
            if String.isEmpty line then
                [ text "\n" ]

            else
                (line
                    |> String.words
                    |> List.map renderWord
                    |> List.intersperse (text " ")
                )
                    ++ [ text "\n" ]
    in
    desc
        |> String.lines
        |> List.concatMap renderLine


loadingPath : Model -> String
loadingPath model =
    case model of
        Loading _ p ->
            p

        _ ->
            ""


loadingCanWebm : Model -> Bool
loadingCanWebm model =
    case model of
        Loading cw _ ->
            cw

        _ ->
            True


{-| Map a file path's extension to a MIME type so the browser can skip
formats it cannot decode without downloading the file to probe it.
-}
mimeTypeFromPath : String -> String
mimeTypeFromPath path =
    let
        ext =
            path
                |> String.split "."
                |> List.reverse
                |> List.head
                |> Maybe.map String.toLower
                |> Maybe.withDefault ""
    in
    case ext of
        "webm" ->
            "video/webm"

        "mp4" ->
            "video/mp4"

        "mkv" ->
            "video/x-matroska"

        "avi" ->
            "video/x-msvideo"

        "mov" ->
            "video/quicktime"

        _ ->
            "application/octet-stream"


{-| Format a yt-dlp upload\_date string (YYYYMMDD) as YYYY-MM-DD. -}
formatDate : String -> String
formatDate date =
    if String.length date == 8 then
        String.slice 0 4 date
            ++ "-"
            ++ String.slice 4 6 date
            ++ "-"
            ++ String.slice 6 8 date

    else
        date


formatViewCount : Int -> String
formatViewCount n =
    if n >= 1000000 then
        String.fromFloat (toFloat (n // 100000) / 10) ++ "M"

    else if n >= 1000 then
        String.fromFloat (toFloat (n // 100) / 10) ++ "K"

    else
        String.fromInt n


formatDuration : Float -> String
formatDuration secs =
    let
        total =
            round secs

        hours =
            total // 3600

        minutes =
            (total - hours * 3600) // 60

        seconds =
            total - hours * 3600 - minutes * 60

        pad n =
            if n < 10 then
                "0" ++ String.fromInt n

            else
                String.fromInt n
    in
    if hours > 0 then
        String.fromInt hours ++ ":" ++ pad minutes ++ ":" ++ pad seconds

    else
        String.fromInt minutes ++ ":" ++ pad seconds


mediaErrorDecoder : D.Decoder MediaErrorCode
mediaErrorDecoder =
    D.at [ "target", "error", "code" ] D.int
        |> D.map
            (\code ->
                case code of
                    1 ->
                        ErrAborted

                    2 ->
                        ErrNetwork

                    3 ->
                        ErrDecode

                    4 ->
                        ErrSrcNotSupported

                    _ ->
                        ErrUnknown code
            )


mediaErrorMessage : MediaErrorCode -> String
mediaErrorMessage code =
    case code of
        ErrAborted ->
            "Playback was aborted."

        ErrNetwork ->
            "A network error prevented the video from loading."

        ErrDecode ->
            "The video could not be decoded (codec error)."

        ErrSrcNotSupported ->
            "This video format is not supported by your browser (AV1/WebM)."

        ErrUnknown n ->
            "Playback failed (error code " ++ String.fromInt n ++ ")."
