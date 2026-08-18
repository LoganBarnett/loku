module Page.Player exposing (Model, Msg(..), init, mediaErrorMessage, update, view)

import Api exposing (VideoItem)
import Html exposing (..)
import Html.Attributes exposing (..)
import Html.Events exposing (on)
import Http
import Json.Decode as D
import Route


type Model
    = Loading Route.PlayerParams
    | Loaded PlayerState
    | Failed String


type alias PlayerState =
    { item : VideoItem
    , bufferFraction : Float
    , mediaError : Maybe MediaErrorCode
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
    = GotItem (Result Http.Error VideoItem)
    | VideoCanPlay
    | VideoProgress Float
    | MediaError MediaErrorCode


init : Route.PlayerParams -> ( Model, Cmd Msg )
init params =
    ( Loading params
    , Api.getItem params.library params.path GotItem
    )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotItem (Ok item) ->
            ( Loaded
                { item = item
                , bufferFraction = 0
                , mediaError = Nothing
                }
            , Cmd.none
            )

        GotItem (Err _) ->
            -- The file may exist but not be indexed yet (mid-scan); fall back
            -- to a bare playback attempt rather than a dead end.
            ( Loaded
                { item = fallbackItem model
                , bufferFraction = 0
                , mediaError = Nothing
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


fallbackItem : Model -> VideoItem
fallbackItem model =
    let
        ( library, path ) =
            case model of
                Loading params ->
                    ( params.library, params.path )

                _ ->
                    ( "", "" )
    in
    { library = library
    , path = path
    , name = path
    , title = Nothing
    , titleSource = Nothing
    , durationSecs = Nothing
    , uploadDate = Nothing
    , year = Nothing
    , description = Nothing
    , channel = Nothing
    , channelUrl = Nothing
    , webpageUrl = Nothing
    , viewCount = Nothing
    , genres = []
    , thumbPath = Nothing
    , compatPath = Nothing
    , nativeType = Nothing
    , derivation = { state = Api.Unknown, error = Nothing }
    , discSet = Nothing
    , discTitleIndex = Nothing
    }



-- VIEW


view : Model -> Html Msg
view model =
    case model of
        Loading _ ->
            p [ class "status-note" ] [ text "Loading…" ]

        Failed err ->
            p [ class "error-note" ] [ text ("Error: " ++ err) ]

        Loaded state ->
            viewLoaded state


viewLoaded : PlayerState -> Html Msg
viewLoaded state =
    div [ class "page" ]
        ([ backLink state.item
         , viewMain state
         , h2 [ class "player-title" ]
            [ text (Maybe.withDefault state.item.name state.item.title) ]
         ]
            ++ viewMetadata state.item
        )


{-| The main panel: the player when playback stands a chance, or an honest
status panel when it does not. A video with neither a typed native source
nor a compat copy but with a conversion underway gets the "being prepared"
panel instead of a doomed playback attempt.
-}
viewMain : PlayerState -> Html Msg
viewMain state =
    case state.mediaError of
        Just code ->
            viewMediaError state.item code

        Nothing ->
            if state.item.nativeType == Nothing && state.item.compatPath == Nothing then
                case state.item.derivation.state of
                    Api.Pending ->
                        viewPreparing
                            "A browser-compatible version is queued for conversion."
                            state.item

                    Api.Processing ->
                        viewPreparing
                            "A browser-compatible version is being prepared right now."
                            state.item

                    Api.Failed ->
                        viewPreparing
                            ("Converting this video failed"
                                ++ (state.item.derivation.error
                                        |> Maybe.map (\e -> ": " ++ e)
                                        |> Maybe.withDefault "."
                                   )
                            )
                            state.item

                    _ ->
                        viewPlayer state

            else
                viewPlayer state


viewPreparing : String -> VideoItem -> Html Msg
viewPreparing message item =
    div [ class "player-panel" ]
        [ p [] [ text message ]
        , downloadLink item
        ]


backLink : VideoItem -> Html Msg
backLink item =
    a
        [ href
            (Route.toString
                (Route.Browse
                    { library = item.library
                    , path = parentPath item.path
                    , page = 1
                    }
                )
            )
        , class "player-back"
        ]
        [ text "← Back" ]


downloadLink : VideoItem -> Html Msg
downloadLink item =
    a
        [ href (Api.videoUrl item.library item.path)
        , attribute "download" ""
        ]
        [ text "Download to play in VLC or another media player" ]


viewMediaError : VideoItem -> MediaErrorCode -> Html Msg
viewMediaError item code =
    div [ class "player-panel" ]
        [ p [ class "error-text" ] [ text (mediaErrorMessage code) ]
        , downloadLink item
        ]


viewPlayer : PlayerState -> Html Msg
viewPlayer state =
    let
        ( extraAttrs, sources ) =
            videoSources state.item
    in
    div []
        [ bufferBar state.bufferFraction
        , video (videoAttrs ++ extraAttrs) sources
        ]


videoAttrs : List (Attribute Msg)
videoAttrs =
    [ controls True
    , on "canplay" (D.succeed VideoCanPlay)
    , class "player-video"
    ]


{-| Build the extra attributes and child source elements for the video
tag from the two facts the server derives: `nativeType` (the codecs-qualified
`<source type>` for the native file, or Nothing when it must not be offered)
and `compatPath` (a universally-playable H.264/AAC MP4, when one exists).

The native source, when offered, carries a full RFC 6381 codecs string so the
browser's canPlayType decides authoritatively — an incapable browser (e.g.
Safari facing AV1/VP9) skips it without downloading a byte and drops to the
compat, while a capable browser plays the higher-quality native. The compat is
the terminal fallback with a bare `video/mp4` type, which every browser
accepts.

-}
videoSources : VideoItem -> ( List (Attribute Msg), List (Html Msg) )
videoSources item =
    let
        nativeUrl =
            Api.videoUrl item.library item.path

        compatUrl compat =
            Api.videoUrl item.library compat

        -- Source elements do not expose target.error.code, so a source-level
        -- failure reports the fixed "not supported" code.
        fixedError =
            on "error" (D.succeed (MediaError ErrSrcNotSupported))

        -- The video element does expose target.error.code, so a src-level
        -- failure reports the specific decode/network/format cause.
        codedError =
            on "error" (D.map MediaError mediaErrorDecoder)
    in
    case ( item.nativeType, item.compatPath ) of
        ( Just nt, Just cp ) ->
            ( []
            , [ source [ src nativeUrl, type_ nt ] []
              , source [ src (compatUrl cp), type_ "video/mp4", fixedError ] []
              ]
            )

        ( Just nt, Nothing ) ->
            -- Only the native, but its codecs type still lets an incapable
            -- browser fail fast instead of downloading it.
            ( []
            , [ source [ src nativeUrl, type_ nt, fixedError ] [] ]
            )

        ( Nothing, Just cp ) ->
            -- Native cannot be offered safely (non-standard container or
            -- unknown codecs); serve only the guaranteed-playable compat.
            ( [ src (compatUrl cp), codedError ], [] )

        ( Nothing, Nothing ) ->
            -- Best effort with no hint: let the browser probe the native and
            -- surface the specific error if it cannot play it.
            ( [ src nativeUrl, codedError ], [] )


{-| Buffering progress as a semantic progress element; a fully buffered
video needs no meter at all.
-}
bufferBar : Float -> Html msg
bufferBar fraction =
    if fraction < 1 then
        progress
            [ class "buffer-progress"
            , attribute "max" "1"
            , attribute "value" (String.fromFloat fraction)
            ]
            []

    else
        text ""


viewMetadata : VideoItem -> List (Html Msg)
viewMetadata item =
    List.filterMap identity
        [ item.year
            |> Maybe.map (\y -> p [] [ text ("Year: " ++ String.fromInt y) ])
        , item.uploadDate
            |> Maybe.map (\d -> p [] [ text ("Uploaded: " ++ formatDate d) ])
        , item.durationSecs
            |> Maybe.map (\s -> p [] [ text ("Duration: " ++ formatDuration s) ])
        , item.viewCount
            |> Maybe.map (\n -> p [] [ text ("Views: " ++ formatViewCount n) ])
        , (if List.isEmpty item.genres then
            Nothing

           else
            Just (String.join ", " item.genres)
          )
            |> Maybe.map (\g -> p [] [ text ("Genres: " ++ g) ])
        , item.channel
            |> Maybe.map (viewChannel item.channelUrl)
        , item.webpageUrl
            |> Maybe.map viewYoutubeLink
        , item.description
            |> Maybe.map viewDescription
        ]


viewChannel : Maybe String -> String -> Html msg
viewChannel channelUrl ch =
    p []
        [ text "Channel: "
        , case channelUrl of
            Just url ->
                a [ href url, target "_blank", attribute "rel" "noopener noreferrer" ]
                    [ text ch ]

            Nothing ->
                text ch
        ]


viewYoutubeLink : String -> Html msg
viewYoutubeLink url =
    p []
        [ a [ href url, target "_blank", attribute "rel" "noopener noreferrer" ]
            [ text "Watch on YouTube" ]
        ]


viewDescription : String -> Html msg
viewDescription desc =
    div [ class "description" ] (renderDescription desc)



-- HELPERS


parentPath : String -> String
parentPath path =
    path
        |> String.split "/"
        |> List.reverse
        |> List.drop 1
        |> List.reverse
        |> String.join "/"


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
links. Newlines are preserved by the description style's pre-wrap.
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


{-| Format a yt-dlp upload\_date string (YYYYMMDD) as YYYY-MM-DD.
-}
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
            "This video format is not supported by your browser."

        ErrUnknown n ->
            "Playback failed (error code " ++ String.fromInt n ++ ")."
